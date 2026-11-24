//! 6-DOF rigid-body assembly mate solver.
//!
//! Free component poses are corrected with damped Levenberg-Marquardt so that
//! coaxial, coincident, distance, parallel, and angle mates are satisfied.
//! Grounded components stay fixed. A mate set that cannot be satisfied is
//! reported through [`SolveReport::conflicting_mates`].

#![forbid(unsafe_code)]

pub mod mates;
pub mod pose;
pub mod solver;

pub use mates::{AssemblyMate, AssemblyMateKind, MateAnchor};
pub use pose::{AssemblyComponent, RigidPose};
pub use solver::{AssemblySolverError, AssemblySystem, SolveConfig, SolveReport};
