use std::marker::PhantomData;

use cubecl_core as cubecl;
use cubecl_core::prelude::*;

use crate::matmul::components::stage::shared::{
    stage_matmul_size, CommonStageConfig, CommonStageInput,
};
use crate::matmul::components::stage::StageMatmulFamily;
use crate::matmul::components::tile::{TileMatmul, TileMatmulFamily};
use crate::matmul::components::{InvalidConfigError, MatmulPrecision, MatmulSize};
use crate::matmul::kernels::MatmulAvailabilityError;
use crate::matmul::{
    components::{
        global::{self, AccumulatorLoader},
        stage::{self, StageConfig as _, StageWriter},
        Ident, MatmulConfigFactory, MatmulProblem, StageDim,
    },
    kernels::matmul::{create_stage_dim, AdvancedConfig},
};

use super::{LhsBufferReader, LhsBufferReaderFamily, RhsBufferReader, RhsBufferReaderFamily};

pub struct SingleBufferMatmulFamily<TMM: TileMatmulFamily> {
    _instruction: PhantomData<TMM>,
}

impl<TMM: TileMatmulFamily> StageMatmulFamily for SingleBufferMatmulFamily<TMM> {
    fn size(config: &Self::Config) -> MatmulSize {
        let tmm_config = config.to_tmm_config();
        stage_matmul_size::<TMM>(&tmm_config, &config.num_stage)
    }

    fn num(config: &Self::Config) -> MatmulSize {
        config.num_stage
    }

    type LhsReader = LhsBufferReaderFamily;
    type RhsReader = RhsBufferReaderFamily;
    type Matmul<I: Numeric, O: Numeric, Acc: Numeric> =
        SingleBufferMatmul<I, O, Acc, TMM::Matmul<I, Acc>>;
}

impl<TMM> MatmulConfigFactory for SingleBufferMatmulFamily<TMM>
where
    TMM: TileMatmulFamily,
{
    type Input = CommonStageInput<TMM>;
    type Config = CommonStageConfig<TMM::Config>;

    fn check_config(config: &Self::Config) -> Result<(), InvalidConfigError> {
        TMM::check_config(&config.to_tmm_config())
    }

    fn check_availability<R: Runtime, MP: MatmulPrecision>(
        client: &ComputeClient<R::Server, R::Channel>,
        config: &Self::Config,
    ) -> Result<(), MatmulAvailabilityError> {
        TMM::check_availability::<R, MP>(client, &config.tmm_config)
    }

    fn make_config(
        input: Self::Input,
        problem: &MatmulProblem,
        cube_dim: &CubeDim,
        cube_count: &CubeCount,
        advanced_config: &AdvancedConfig,
    ) -> Self::Config {
        let tile = input.tile;

        let tmm_config = TMM::make_config(tile, problem, cube_dim, cube_count, advanced_config);
        let tmm_size = TMM::size(&tmm_config);
        let stage_size = stage_matmul_size::<TMM>(&tmm_config, &input.num_stages);

        let (tile_m, tile_n, tile_k) = (tmm_size.m, tmm_size.n, tmm_size.k);
        let (lhs_stage_dim, rhs_stage_dim, out_stage_dim) = create_stage_dim(
            stage_size.m,
            stage_size.n,
            stage_size.k,
            tile_m,
            tile_n,
            tile_k,
        );

        CommonStageConfig::new(
            tmm_config,
            input.num_stages,
            lhs_stage_dim,
            rhs_stage_dim,
            out_stage_dim,
            lhs_stage_dim.num_tiles_x_dim(),
            advanced_config.lhs_tiling_order,
            advanced_config.rhs_tiling_order,
        )
    }
}

/// Performs matrix multiplication at the stage level, where each plane is responsible for a row of tiles:
/// - One plane per tile in m dimension,
/// - One accumulator per tile in n dimension
///
/// Very similar to multi buffer, except is unable to have more than one buffer, and takes BufferReaders for StageReaders
///
/// # Assumptions
/// - There are at least as many planes as the stage size in m
pub struct SingleBufferMatmul<I: Numeric, O: Numeric, EA: Numeric, TMM: TileMatmul<I, EA>> {
    _input_precision: PhantomData<I>,
    _output_precision: PhantomData<O>,
    _accumulator_precision: PhantomData<EA>,
    _instruction: PhantomData<TMM>,
}

#[cube]
impl<I, O, EA, TMM> stage::StageMatmul<I, O, EA> for SingleBufferMatmul<I, O, EA, TMM>
where
    I: Numeric,
    O: Numeric,
    EA: Numeric,
    TMM: TileMatmul<I, EA>,
{
    type Config = CommonStageConfig<TMM::Config>;
    type LhsReader = LhsBufferReader<I>;
    type RhsReader = RhsBufferReader<I>;
    type Accumulator = Sequence<TMM::Accumulator>;
    type LhsTile = TMM::Lhs;
    type RhsTile = TMM::Rhs;

    fn execute(
        lhs_reader: &LhsBufferReader<I>,
        rhs_reader: &RhsBufferReader<I>,
        lhs_tile: &mut Self::LhsTile,
        rhs_tile: &mut Self::RhsTile,
        acc: &mut Self::Accumulator,
        #[comptime] config: Self::Config,
    ) {
        let tile_lhs = LhsBufferReader::read_tile::<TMM::Config>(lhs_reader, UNIT_POS_Y, config);
        TMM::fill_lhs(&tile_lhs, lhs_tile, config.to_tmm_config());

        #[unroll]
        for accumulator_iter in 0..acc.len() {
            let tile_rhs =
                RhsBufferReader::read_tile::<TMM::Config>(rhs_reader, accumulator_iter, config);
            TMM::fill_rhs(&tile_rhs, rhs_tile, config.to_tmm_config());

            let accumulator = acc.index_mut(accumulator_iter);
            TMM::execute(lhs_tile, rhs_tile, accumulator, config.to_tmm_config());
        }
    }

    fn init_tile_inputs(#[comptime] config: Self::Config) -> (TMM::Lhs, TMM::Rhs) {
        (
            TMM::allocate_lhs(config.to_tmm_config()),
            TMM::allocate_rhs(config.to_tmm_config()),
        )
    }

    fn init_accumulator(#[comptime] config: Self::Config) -> Self::Accumulator {
        let mut accumulators = Sequence::<TMM::Accumulator>::new();

        #[unroll]
        for _ in 0..config.num_stage.n {
            accumulators.push(TMM::allocate_accumulator(config.to_tmm_config()));
        }

        accumulators
    }

    fn zero_accumulator(acc: &mut Self::Accumulator, #[comptime] config: Self::Config) {
        #[unroll]
        for i in 0..config.num_stage.n {
            TMM::zero_accumulator(acc.index_mut(i), config.to_tmm_config());
        }
    }

    fn fill_accumulator<L: AccumulatorLoader<O, EA, Self::Config>>(
        loader: &mut L,
        acc: &mut Self::Accumulator,
        #[comptime] config: Self::Config,
    ) {
        #[unroll]
        for i in 0..config.num_stage.n {
            let acc = acc.index_mut(i);
            L::load::<I, TMM>(loader, acc, i, config.to_tmm_config());
        }
    }

    fn read_accumulator<SW: StageWriter<O>, G: global::GlobalConfig>(
        acc: &Self::Accumulator,
        out: &mut SW,
        #[comptime] stage_config: Self::Config,
        #[comptime] global_config: G,
    ) {
        let out_smem_line_size = global_config.stage_line_size(Ident::Out);
        let num_tile_lines =
            stage_config.stage_dim(Ident::Out).tile_num_elements() / out_smem_line_size;

        let start = num_tile_lines * UNIT_POS_Y;
        let mut out_smem = SharedMemory::<O>::new_lined(
            num_tile_lines * stage_config.num_planes(),
            out_smem_line_size,
        );

        #[unroll]
        for accumulator_iter in 0..acc.len() {
            let accumulator = acc.index(accumulator_iter);
            let mut smem_slice = out_smem.slice_mut(start, start + num_tile_lines);
            TMM::read_accumulator(accumulator, &mut smem_slice, stage_config.to_tmm_config());
            SW::write::<O, G>(
                out,
                smem_slice.to_slice(),
                UNIT_POS_Y,
                accumulator_iter,
                global_config,
            );
        }
    }
}
