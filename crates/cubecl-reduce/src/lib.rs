mod config;
mod error;
mod instructions;
mod launch;
mod strategy;

pub use config::*;
pub use error::*;
pub use instructions::*;
pub use launch::*;
pub use strategy::*;

#[cfg(feature = "export_tests")]
pub mod test;

use cubecl_core::prelude::*;

/// Entry point for reduce.
pub fn reduce<R: Runtime, In: Numeric, Out: Numeric, Inst: ReduceInstruction<In>>(
    client: &ComputeClient<R::Server, R::Channel>,
    input: TensorHandleRef<R>,
    output: TensorHandleRef<R>,
    axis: u32,
    strategy: Option<ReduceStrategy>,
) -> Result<(), ReduceError> {
    let strategy = strategy
        .map(|s| s.validate::<R>(client))
        .unwrap_or(Ok(ReduceStrategy::fallback_strategy::<R>(client)))?;
    let (cube_count, cube_dim, config) =
        generate_config::<R>(client, &input, &output, axis, &strategy);
    launch_reduce::<R, In, Out, Inst>(
        client, input, output, axis, cube_count, cube_dim, config, strategy,
    );
    Ok(())
}
