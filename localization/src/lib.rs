//! This crate provides a node that can digest multiple streams
//! of spatial input to determine where an object (presumably a
//! robot) is in global space.

use std::{
    num::NonZeroUsize,
    time::Duration,
};

use frames::{IMUFrame, OrientationFrame, PositionFrame, VelocityFrame};
use fxhash::FxHashMap;
use nalgebra::{
    convert as nconvert, UnitQuaternion, Vector3
};
use rig::{RobotBase, RobotElementRef};
use smach::State;
use unros::{
    anyhow, async_trait,
    pubsub::{subs::DirectSubscription, Subscriber},
    Node, NodeIntrinsics, RuntimeContext,
};
use utils::{UnorderedQueue, random_unit_vector};
use calib::calibrate_localizer;
use run::run_localizer;

pub mod frames;
mod utils;
mod calib;
mod run;

pub use utils::{Float, gravity};

/// A Node that can digest multiple streams of spatial input to
/// determine where an object is in global space.
///
/// Processing does not occur until the node is running.
pub struct Localizer<N: Float> {
    bb: LocalizerBlackboard<N>,
    intrinsics: NodeIntrinsics<Self>,
}

impl<N: Float> Localizer<N> {
    pub fn new(robot_base: RobotBase, start_variance: N) -> Self {
        Self {
            bb: LocalizerBlackboard {
                point_count: NonZeroUsize::new(500).unwrap(),
                start_std_dev: start_variance.sqrt(),
                calibration_duration: Duration::from_secs(3),
                recalibrate_sub: Subscriber::new(1),
                minimum_unnormalized_weight: nconvert(0.6),
                undeprivation_factor: nconvert(0.05),
                likelihood_table: LikelihoodTable::default(),
                imu_sub: Subscriber::new(1),
                position_sub: Subscriber::new(1),
                orientation_sub: Subscriber::new(1),
                velocity_sub: Subscriber::new(1),
                robot_base,
                max_delta: Duration::from_millis(50),
                linear_acceleration_std_dev_count: 10,
                angular_velocity_std_dev_count: 10,
                linear_acceleration_std_devs: std::iter::repeat(N::zero()).take(10).collect(),
                angular_velocity_std_devs: std::iter::repeat(N::zero()).take(10).collect(),
                calibrations: Default::default(),
                context: None,
                start_orientation: UnitQuaternion::default(),
            },
            intrinsics: Default::default(),
        }
    }

    /// Provide an imu subscription.
    ///
    /// Some messages may be skipped if there are too many.
    pub fn create_imu_sub(&self) -> DirectSubscription<IMUFrame<N>> {
        self.bb.imu_sub.create_subscription()
    }

    /// Provide a position subscription.
    ///
    /// Some messages may be skipped if there are too many.
    pub fn create_position_sub(&self) -> DirectSubscription<PositionFrame<N>> {
        self.bb.position_sub.create_subscription()
    }

    /// Provide a velocity subscription.
    ///
    /// Some messages may be skipped if there are too many.
    pub fn create_velocity_sub(&self) -> DirectSubscription<VelocityFrame<N>> {
        self.bb.velocity_sub.create_subscription()
    }

    /// Provide an orientation subscription.
    ///
    /// Some messages may be skipped if there are too many.
    pub fn create_orientation_sub(&self) -> DirectSubscription<OrientationFrame<N>> {
        self.bb.orientation_sub.create_subscription()
    }
}

struct CalibratingImu<N: Float> {
    count: usize,
    accel: Vector3<N>,
    angular_velocity: UnitQuaternion<N>,
}

#[derive(Debug)]
struct CalibratedImu<N: Float> {
    accel_scale: N,
    accel_correction: UnitQuaternion<N>,
    angular_velocity_bias: UnitQuaternion<N>,
}

pub struct LikelihoodTable<N: Float> {
    pub position: Box<dyn Fn(Vector3<N>) -> N + Send + Sync>,
    pub linear_velocity: Box<dyn Fn(Vector3<N>) -> N + Send + Sync>,
    pub linear_acceleration: Box<dyn Fn(Vector3<N>) -> N + Send + Sync>,

    pub orientation: Box<dyn Fn(UnitQuaternion<N>) -> N + Send + Sync>,
    pub angular_velocity: Box<dyn Fn(UnitQuaternion<N>) -> N + Send + Sync>,
}

impl<N: Float> Default for LikelihoodTable<N> {
    fn default() -> Self {
        Self {
            position: Box::new(|_| N::one()),
            linear_velocity: Box::new(|_| N::one()),
            linear_acceleration: Box::new(|_| N::one()),
            orientation: Box::new(|_| N::one()),
            angular_velocity: Box::new(|_| N::one()),
        }
    }
}

pub struct LocalizerBlackboard<N: Float> {
    pub point_count: NonZeroUsize,
    pub start_std_dev: N,
    pub max_delta: Duration,

    pub minimum_unnormalized_weight: N,
    pub undeprivation_factor: N,

    pub linear_acceleration_std_dev_count: usize,
    pub angular_velocity_std_dev_count: usize,

    linear_acceleration_std_devs: UnorderedQueue<N>,
    angular_velocity_std_devs: UnorderedQueue<N>,

    pub calibration_duration: Duration,
    pub likelihood_table: LikelihoodTable<N>,

    recalibrate_sub: Subscriber<()>,
    calibrations: FxHashMap<RobotElementRef, CalibratedImu<N>>,

    imu_sub: Subscriber<IMUFrame<N>>,
    position_sub: Subscriber<PositionFrame<N>>,
    velocity_sub: Subscriber<VelocityFrame<N>>,
    orientation_sub: Subscriber<OrientationFrame<N>>,

    start_orientation: UnitQuaternion<N>,

    robot_base: RobotBase,

    context: Option<RuntimeContext>,
}

#[async_trait]
impl<N: Float> Node for Localizer<N> {
    const DEFAULT_NAME: &'static str = "positioning";

    fn get_intrinsics(&mut self) -> &mut NodeIntrinsics<Self> {
        &mut self.intrinsics
    }

    async fn run(mut self, context: RuntimeContext) -> anyhow::Result<()> {
        self.bb.context = Some(context);

        let (calib, calib_trans) = State::new(calibrate_localizer);
        let (run, run_trans) = State::new(run_localizer);

        let start_state = calib.clone();

        calib_trans.set_transition(move |_| Some(run.clone()));
        run_trans.set_transition(move |_| Some(calib.clone()));

        start_state.start(self.bb).await;
        unreachable!()
    }
}


impl<N: Float> std::ops::Deref for Localizer<N> {
    type Target = LocalizerBlackboard<N>;

    fn deref(&self) -> &Self::Target {
        &self.bb
    }
}


impl<N: Float> std::ops::DerefMut for Localizer<N> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.bb
    }
}