use cubecl_core as cubecl;
use cubecl_core::prelude::*;

use super::{Reduce, ReduceInstruction};

pub struct Prod;

#[cube]
impl<In: Numeric> ReduceInstruction<In> for Prod {}

#[cube]
impl<In: Numeric> Reduce<In> for Prod {
    type Accumulator = Line<In>;

    fn init_accumulator(#[comptime] line_size: u32) -> Self::Accumulator {
        Line::empty(line_size).fill(In::from_int(1))
    }

    fn reduce(
        accumulator: &mut Self::Accumulator,
        item: Line<In>,
        _coordinate: Line<u32>,
        #[comptime] use_plane: bool,
    ) {
        if use_plane {
            *accumulator *= plane_prod(item);
        } else {
            *accumulator *= item;
        }
    }

    fn merge_line<Out: Numeric>(accumulator: Self::Accumulator, _shape_axis_reduce: u32) -> Out {
        let mut prod = In::from_int(1);
        #[unroll]
        for k in 0..accumulator.size() {
            prod *= accumulator[k];
        }
        Out::cast_from(prod)
    }

    fn to_output_perpendicular<Out: Numeric>(
        accumulator: Self::Accumulator,
        _shape_axis_reduce: u32,
    ) -> Line<Out> {
        Line::cast_from(accumulator)
    }
}