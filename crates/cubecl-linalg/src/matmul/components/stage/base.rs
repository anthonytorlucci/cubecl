use cubecl_core as cubecl;
use cubecl_core::prelude::*;

use crate::matmul::components::tile::TileConfig;
use crate::matmul::components::{config::MatmulConfig, global::AccumulatorLoader};
use crate::matmul::components::{global, MatmulConfigFactory};
use crate::matmul::components::{Ident, MatrixLayout};
use crate::matmul::components::{MatmulSize, StageDim};

use super::tiling_order::TilingOrderConfig;

pub trait ReaderFamily {
    type Reader<I: Numeric>: CubeType;
}

pub trait StageMatmulFamily: MatmulConfigFactory<Config: Config> + Send + Sync + 'static {
    type LhsReader: ReaderFamily;
    type RhsReader: ReaderFamily;

    fn size(config: &Self::Config) -> MatmulSize;
    /// Return the number of matmuls computed by the stage.
    fn num(config: &Self::Config) -> MatmulSize;

    type Matmul<I: Numeric, O: Numeric, Acc: Numeric>: Matmul<
        I,
        O,
        Acc,
        Config = Self::Config,
        LhsReader = <Self::LhsReader as ReaderFamily>::Reader<I>,
        RhsReader = <Self::RhsReader as ReaderFamily>::Reader<I>,
    >;
}

#[cube]
/// Provides matrix multiplication operations at the stage level.
///
/// At the stage level,
///  - Inputs are staged into an intermediate memory called stage (typically a shared memory).
///  - All planes within a Cube can collaborate to solve the problem
///  - Dimensions M, N and K are fixed to an integer, and the
///    matrix multiplication works only for size (M, K) · (K, N) = (M, N).
///    These integers are multiples of the underlying Tile matmul,
///    corresponding to the number of tiles in each dimension.
///
/// Assumptions:
///  - Data given as inputs by stage readers must always be valid. If the actual matrix multiplication
///    should be done on smaller sizes than M, N and K, padding with zeros must be done beforehand.
///  - Enough planes are launched to perform the whole computation
pub trait Matmul<I: Numeric, O: Numeric, Acc: Numeric>: 'static + Send + Sync {
    type Config: Config;
    // /// Number of rows of LHS
    // const M: u32;
    // /// Number of columns of RHS
    // const N: u32;
    // /// Common dimension of LHS and RHS
    // const K: u32;

    /// Contains the matrix multiplication output, that can be shared across the different planes of the cube.
    /// The same Accumulator will be added to across multiple executions of the stage matmul.
    type Accumulator: CubeType;

    type LhsReader: CubeType;
    type RhsReader: CubeType;

    type LhsTile: CubeType;
    type RhsTile: CubeType;

    /// Executes the matrix multiplication of LHS and RHS, adding the result to the accumulator
    fn execute(
        lhs: &Self::LhsReader,
        rhs: &Self::RhsReader,
        instruction_lhs: &mut Self::LhsTile,
        instruction_rhs: &mut Self::RhsTile,
        acc: &mut Self::Accumulator,
        #[comptime] config: Self::Config,
    );

    fn init_tile_inputs(#[comptime] config: Self::Config) -> (Self::LhsTile, Self::RhsTile);

    /// Reads the result of the accumulator and hands it to the stage writer
    fn read_accumulator<Out: StageWriter<O>, G: global::GlobalConfig>(
        acc: &Self::Accumulator,
        out: &mut Out,
        #[comptime] stage_config: Self::Config,
        #[comptime] global_config: G,
    );

    /// Create an instance of the accumulator, without data
    fn init_accumulator(#[comptime] config: Self::Config) -> Self::Accumulator;

    /// Fill the accumulator with zeros
    fn zero_accumulator(acc: &mut Self::Accumulator, #[comptime] config: Self::Config);

    /// Fill the accumulator with data
    fn fill_accumulator<L: AccumulatorLoader<O, Acc, Self::Config>>(
        loader: &mut L,
        acc: &mut Self::Accumulator,
        #[comptime] config: Self::Config,
    );
}

#[cube]
/// Input to the stage matmul, responsible of handing slices of data
/// at precise locations in the stage
pub trait StageReader<ES: Numeric>: CubeType {
    /// Hands a portion of data from the stage, whose location is function of the
    /// plane, buffer and accumulator indexes.
    fn read_tile<S: Config>(
        this: &Self,
        compute_plane_offset: u32,
        buffer_offset: u32,
        accumulator_offset: u32,
        #[comptime] config: S,
    ) -> Slice<Line<ES>>;
}

#[cube]
/// Responsible of writing the accumulated stage matmul output
/// to global memory
pub trait StageWriter<EG: Numeric>: CubeType + 'static + Send + Sync {
    /// Writes the given slice to global memory, at a position that depends on
    /// plane and accumulator indexes.
    fn write<ES: Numeric, G: global::GlobalConfig>(
        this: &mut Self,
        slice: Slice<Line<ES>>,
        compute_plane_offset: u32,
        accumulator_offset: u32,
        #[comptime] config: G,
    );
}

/// Configuration for the Stage matmul (SMM) level
pub trait Config: MatmulConfig {
    /// Underlying Tile matmul config
    type TmmConfig: TileConfig;

    /// Convert itself to the underlying tile matmul config
    fn to_tmm_config(self) -> Self::TmmConfig;

    /// Returns the line size for the given ident
    fn line_size(&self, ident: Ident) -> u32;

    /// Returns the [StageDim] for the given ident
    fn stage_dim(&self, ident: Ident) -> Box<dyn StageDim>;

    /// Returns the [MatrixLayout] for the given ident
    fn layout(&self, ident: Ident) -> MatrixLayout;

    /// Returns the number of planes in the cube
    fn num_planes(&self) -> u32;

    /// Returns the size of the plane dimension
    fn plane_dim(&self) -> u32;

    /// Returns the order in which tiles should be loaded to the stage
    fn tiling_order(&self, ident: Ident) -> TilingOrderConfig;
}
