use std::mem::transmute;

use crate::{BasicBlock, BlockUse, NodeIndex, Optimizer};
use cubecl_core::ir::{
    BinaryOperator, Branch, ConstantScalarValue, Elem, If, IfElse, Item, Loop, Operator, RangeLoop,
    Switch, UnaryOperator, Variable,
};
use petgraph::visit::EdgeRef;

/// Control flow that terminates a block
#[derive(Default, Debug, Clone)]
pub enum ControlFlow {
    /// An if or if-else branch that should be structured if applicable.
    IfElse {
        cond: Variable,
        then: NodeIndex,
        or_else: NodeIndex,
        merge: Option<NodeIndex>,
    },
    /// A switch branch that paths based on `value`
    Switch {
        value: Variable,
        default: NodeIndex,
        branches: Vec<(u32, NodeIndex)>,
        merge: Option<NodeIndex>,
    },
    /// A loop with a header (the block that contains this variant), a `body` and a `continue target`.
    /// `merge` is the block that gets executed as soon as the loop terminates.
    Loop {
        body: NodeIndex,
        continue_target: NodeIndex,
        merge: NodeIndex,
    },
    /// A loop with a header (the block that contains this variant), a `body` and a `continue target`.
    /// `merge` is the block that gets executed as soon as the loop terminates. The header contains
    /// the break condition.
    LoopBreak {
        break_cond: Variable,
        body: NodeIndex,
        continue_target: NodeIndex,
        merge: NodeIndex,
    },
    /// A return statement. This should only occur once in the program and all other returns should
    /// instead branch to this single return block.
    Return,
    /// No special control flow. The block must have exactly one edge that should be followed.
    #[default]
    None,
}

impl Optimizer {
    pub(crate) fn parse_control_flow(&mut self, branch: Branch) {
        match branch {
            Branch::If(if_) => self.parse_if(if_),
            Branch::IfElse(if_else) => self.parse_if_else(if_else),
            Branch::Select(mut select) => {
                self.find_writes_select(&mut select);
                self.current_block_mut()
                    .ops
                    .borrow_mut()
                    .push(Branch::Select(select).into());
            }
            Branch::Switch(switch) => self.parse_switch(switch),
            Branch::RangeLoop(range_loop) => {
                self.parse_for_loop(range_loop);
            }
            Branch::Loop(loop_) => self.parse_loop(loop_),
            Branch::Return => {
                let current_block = self.current_block.take().unwrap();
                self.program.add_edge(current_block, self.ret, ());
            }
            Branch::Break => {
                let current_block = self.current_block.take().unwrap();
                let loop_break = self.loop_break.back().expect("Can't break outside loop");
                self.program.add_edge(current_block, *loop_break, ());
            }
        }
    }

    pub(crate) fn parse_if(&mut self, if_: If) {
        let current_block = self.current_block.unwrap();
        let then = self.program.add_node(BasicBlock::default());
        let next = self.program.add_node(BasicBlock::default());
        let mut merge = next;

        self.program.add_edge(current_block, then, ());
        self.program.add_edge(current_block, next, ());

        self.current_block = Some(then);
        let is_break = self.parse_scope(if_.scope);

        if let Some(current_block) = self.current_block {
            self.program.add_edge(current_block, next, ());
        } else {
            // Returned
            merge = self.merge_ret();
        }

        let merge = if is_break { None } else { Some(merge) };

        *self.program[current_block].control_flow.borrow_mut() = ControlFlow::IfElse {
            cond: if_.cond,
            then,
            or_else: next,
            merge,
        };
        if let Some(merge) = merge {
            self.program[merge].block_use.push(BlockUse::Merge);
        }
        self.current_block = Some(next);
    }

    pub(crate) fn parse_if_else(&mut self, if_else: IfElse) {
        let current_block = self.current_block.unwrap();
        let then = self.program.add_node(BasicBlock::default());
        let or_else = self.program.add_node(BasicBlock::default());
        let next = self.program.add_node(BasicBlock::default());
        let mut merge = next;

        self.program.add_edge(current_block, then, ());
        self.program.add_edge(current_block, or_else, ());

        self.current_block = Some(then);
        let is_break = self.parse_scope(if_else.scope_if);

        if let Some(current_block) = self.current_block {
            self.program.add_edge(current_block, next, ());
        } else {
            // Returned
            merge = self.merge_ret();
        }

        self.current_block = Some(or_else);
        let is_break = self.parse_scope(if_else.scope_else) || is_break;

        if let Some(current_block) = self.current_block {
            self.program.add_edge(current_block, next, ());
        } else {
            // Returned
            merge = self.merge_ret();
        }

        let merge = if is_break { None } else { Some(merge) };
        *self.program[current_block].control_flow.borrow_mut() = ControlFlow::IfElse {
            cond: if_else.cond,
            then,
            or_else,
            merge,
        };
        if let Some(merge) = merge {
            self.program[merge].block_use.push(BlockUse::Merge);
        }
        self.current_block = Some(next);
    }

    pub(crate) fn parse_switch(&mut self, switch: Switch) {
        let current_block = self.current_block.unwrap();
        let next = self.program.add_node(BasicBlock::default());

        let branches = switch
            .cases
            .into_iter()
            .map(|(val, case)| {
                let case_id = self.program.add_node(BasicBlock::default());
                self.program.add_edge(current_block, case_id, ());
                self.current_block = Some(case_id);
                let is_break = self.parse_scope(case);
                let is_ret = if let Some(current_block) = self.current_block {
                    self.program.add_edge(current_block, next, ());
                    false
                } else {
                    !is_break
                };
                let val = match val.as_const().expect("Switch value must be constant") {
                    ConstantScalarValue::Int(val, _) => unsafe {
                        transmute::<i32, u32>(val as i32)
                    },
                    ConstantScalarValue::UInt(val) => val as u32,
                    _ => unreachable!("Switch cases must be integer"),
                };
                (val, case_id, is_break, is_ret)
            })
            .collect::<Vec<_>>();

        let is_break_branch = branches.iter().any(|it| it.2);
        let mut is_ret = branches.iter().any(|it| it.3);
        let branches = branches
            .into_iter()
            .map(|it| (it.0, it.1))
            .collect::<Vec<_>>();

        let default = self.program.add_node(BasicBlock::default());
        self.program.add_edge(current_block, default, ());
        self.current_block = Some(default);
        let is_break_def = self.parse_scope(switch.scope_default);

        if let Some(current_block) = self.current_block {
            self.program.add_edge(current_block, next, ());
        } else {
            is_ret = !is_break_def;
        }

        let merge = if is_break_def || is_break_branch {
            None
        } else if is_ret {
            Some(self.merge_ret())
        } else {
            self.program[next].block_use.push(BlockUse::Merge);
            Some(next)
        };

        *self.program[current_block].control_flow.borrow_mut() = ControlFlow::Switch {
            value: switch.value,
            default,
            branches,
            merge,
        };

        self.current_block = Some(next);
    }

    fn parse_loop(&mut self, loop_: Loop) {
        let current_block = self.current_block.unwrap();
        let header = self.program.add_node(BasicBlock::default());
        self.program.add_edge(current_block, header, ());

        let body = self.program.add_node(BasicBlock::default());
        let next = self.program.add_node(BasicBlock::default());

        self.program.add_edge(header, body, ());

        self.loop_break.push_back(next);

        self.current_block = Some(body);
        self.parse_scope(loop_.scope);
        let continue_target = self.program.add_node(BasicBlock::default());
        self.program[continue_target]
            .block_use
            .push(BlockUse::ContinueTarget);

        self.loop_break.pop_back();

        if let Some(current_block) = self.current_block {
            self.program.add_edge(current_block, continue_target, ());
        }

        self.program.add_edge(continue_target, header, ());

        *self.program[header].control_flow.borrow_mut() = ControlFlow::Loop {
            body,
            continue_target,
            merge: next,
        };
        self.program[next].block_use.push(BlockUse::Merge);
        self.current_block = Some(next);
    }

    fn parse_for_loop(&mut self, range_loop: RangeLoop) {
        let step = range_loop
            .step
            .unwrap_or(Variable::ConstantScalar(ConstantScalarValue::UInt(1)));

        let i_id = match range_loop.i {
            Variable::Local { id, depth, .. } => (id, depth),
            _ => unreachable!(),
        };
        let i = range_loop.i;
        self.program.variables.insert(i_id, i.item());

        let mut assign = Operator::Assign(UnaryOperator {
            input: range_loop.start,
            out: i,
        })
        .into();
        self.visit_operation(&mut assign, |_, _| {}, |opt, var| opt.write_var(var));
        self.current_block_mut().ops.borrow_mut().push(assign);

        let current_block = self.current_block.unwrap();
        let header = self.program.add_node(BasicBlock::default());
        self.program.add_edge(current_block, header, ());

        let body = self.program.add_node(BasicBlock::default());
        let next = self.program.add_node(BasicBlock::default());

        self.program.add_edge(header, body, ());
        self.program.add_edge(header, next, ());

        self.loop_break.push_back(next);

        self.current_block = Some(body);
        self.parse_scope(range_loop.scope);

        self.loop_break.pop_back();

        let current_block = self.current_block.expect("For loop has no loopback path");

        let continue_target = if self.program[current_block]
            .block_use
            .contains(&BlockUse::Merge)
        {
            let target = self.program.add_node(BasicBlock::default());
            self.program.add_edge(current_block, target, ());
            target
        } else {
            current_block
        };

        self.program.add_edge(continue_target, header, ());

        self.program[continue_target]
            .block_use
            .push(BlockUse::ContinueTarget);
        self.program[next].block_use.push(BlockUse::Merge);
        self.current_block = Some(next);

        // For loop constructs
        self.program
            .insert_phi(header, i_id, range_loop.start.item());
        {
            let op = match range_loop.inclusive {
                true => Operator::LowerEqual,
                false => Operator::Lower,
            };
            let tmp = self.create_temporary(Item::new(Elem::Bool));
            self.program[header].ops.borrow_mut().push(
                op(BinaryOperator {
                    lhs: i,
                    rhs: range_loop.end,
                    out: tmp,
                })
                .into(),
            );

            *self.program[header].control_flow.borrow_mut() = ControlFlow::LoopBreak {
                break_cond: tmp,
                body,
                continue_target,
                merge: next,
            };
        }
        self.program[current_block].ops.borrow_mut().push(
            Operator::Add(BinaryOperator {
                lhs: i,
                rhs: step,
                out: i,
            })
            .into(),
        );
    }

    pub(crate) fn merge_ret(&mut self) -> NodeIndex {
        if self.program[self.ret].block_use.contains(&BlockUse::Merge) {
            let merge = self.program.add_node(BasicBlock::default());
            self.program.add_edge(merge, self.ret, ());
            self.program[merge].block_use.push(BlockUse::Merge);
            merge
        } else {
            self.program[self.ret].block_use.push(BlockUse::Merge);
            self.ret
        }
    }

    pub(crate) fn split_critical_edges(&mut self) {
        for block in self.node_ids() {
            let successors = self.program.edges(block);
            let successors = successors.map(|edge| (edge.id(), edge.target()));
            let successors: Vec<_> = successors.collect();

            if successors.len() > 1 {
                let crit = successors
                    .iter()
                    .filter(|(_, b)| self.predecessors(*b).len() > 1)
                    .collect::<Vec<_>>();
                for (edge, successor) in crit {
                    self.program.remove_edge(*edge);
                    let new_block = self.program.add_node(BasicBlock::default());
                    self.program.add_edge(block, new_block, ());
                    self.program.add_edge(new_block, *successor, ());
                    update_phi(self, *successor, block, new_block);
                    update_control_flow(self, block, *successor, new_block);
                }
            }
        }
    }
}

fn update_control_flow(opt: &mut Optimizer, block: NodeIndex, from: NodeIndex, to: NodeIndex) {
    let update = |id: &mut NodeIndex| {
        if *id == from {
            *id = to
        }
    };

    match &mut *opt.program[block].control_flow.borrow_mut() {
        ControlFlow::IfElse { then, or_else, .. } => {
            update(then);
            update(or_else);
        }
        ControlFlow::Switch {
            default, branches, ..
        } => {
            update(default);

            for branch in branches {
                update(&mut branch.1);
            }
        }
        _ => {}
    }
}

fn update_phi(opt: &mut Optimizer, block: NodeIndex, from: NodeIndex, to: NodeIndex) {
    for phi in opt.program[block].phi_nodes.borrow_mut().iter_mut() {
        for entry in phi.entries.iter_mut() {
            if entry.block == from {
                entry.block = to;
            }
        }
    }
}