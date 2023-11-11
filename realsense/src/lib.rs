use std::{
    collections::HashSet,
    ops::Deref,
    path::Path,
    sync::{Arc, Mutex},
};

use image::{DynamicImage, ImageBuffer, Rgb};
use nalgebra::Vector3;
use quaternion_core::{to_euler_angles, RotationType, RotationSequence};
use realsense_rust::{
    config::Config,
    context::Context,
    device::Device,
    frame::{ColorFrame, PoseFrame},
    kind::{Rs2CameraInfo, Rs2Format, Rs2StreamKind},
    pipeline::InactivePipeline,
};
use unros_core::{
    anyhow, async_trait, tokio_rayon, Node, signal::{Signal, SignalRef}, RuntimeContext, setup_logging,
};

#[derive(Clone, Copy)]
pub struct IMUFrame {
    pub acceleration: Vector3<f32>,
    pub rotation: Vector3<f32>,
}

pub struct RealSenseCamera {
    device: Device,
    context: Arc<Mutex<Context>>,
    image_received: Signal<Arc<DynamicImage>>,
    imu_received: Signal<IMUFrame>
}

impl RealSenseCamera {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let mut context = Context::new()?;
        let device = context.add_device(path)?;
        Ok(Self {
            device,
            context: Arc::new(Mutex::new(context)),
            image_received: Default::default(),
            imu_received: Default::default(),
        })
    }
    pub fn image_received_signal(&mut self) -> SignalRef<Arc<DynamicImage>> {
        self.image_received.get_ref()
    }
}

#[async_trait]
impl Node for RealSenseCamera {
    const DEFAULT_NAME: &'static str = "realsense";

    async fn run(self, context: RuntimeContext) -> anyhow::Result<()> {
        setup_logging!(context);
        let pipeline = InactivePipeline::try_from(self.context.lock().unwrap().deref())?;
        let mut config = Config::new();

        let usb_cstr = self.device.info(Rs2CameraInfo::UsbTypeDescriptor).unwrap();
        let usb_val: f32 = usb_cstr.to_str().unwrap().parse().unwrap();
        if usb_val >= 3.0 {
            config
                .enable_device_from_serial(self.device.info(Rs2CameraInfo::SerialNumber).unwrap())?
                .disable_all_streams()?
                // .enable_stream(Rs2StreamKind::Depth, None, 640, 0, Rs2Format::Z16, 30)?
                .enable_stream(Rs2StreamKind::Color, None, 640, 0, Rs2Format::Rgb8, 30)?
                .enable_stream(Rs2StreamKind::Gyro, None, 0, 0, Rs2Format::Any, 0)?;
        } else {
            warn!("A Realsense camera is not attached to a USB 3.0 port");
            config
                .enable_device_from_serial(self.device.info(Rs2CameraInfo::SerialNumber).unwrap())?
                .disable_all_streams()?
                // .enable_stream(Rs2StreamKind::Depth, None, 640, 0, Rs2Format::Z16, 30)?
                .enable_stream(Rs2StreamKind::Gyro, None, 0, 0, Rs2Format::Any, 0)?;
        }

        // Change pipeline's type from InactivePipeline -> ActivePipeline
        let mut pipeline = pipeline.start(Some(config))?;

        tokio_rayon::spawn(move || {
            loop {
                let frames = pipeline.wait(None)?;

                // Get depth
                // let mut depth_frames = frames.frames_of_type::<DepthFrame>();
                // if !depth_frames.is_empty() {
                //     let depth_frame = depth_frames.pop().unwrap();
                //     let tmp_distance =
                //         depth_frame.distance(depth_frame.width() / 2, depth_frame.height() / 2)?;
                //     if tmp_distance != 0.0 {
                //         distance = tmp_distance;
                //     }
                // }

                // Get color
                for frame in frames.frames_of_type::<ColorFrame>() {
                    unsafe {
                        let image_size = frame.width() * frame.height() * 3;
                        let ptr: *const _ = frame.get_data();
                        let ptr: *const u8 = ptr.cast();
                        let buf = std::slice::from_raw_parts(ptr, image_size).to_vec();
                        let img = ImageBuffer::<Rgb<u8>, _>::from_raw(
                            frame.width() as u32,
                            frame.height() as u32,
                            buf,
                        )
                        .ok_or_else(|| {
                            anyhow::anyhow!("Failed to convert RealSense color frame into image")
                        })?;
                        let img = DynamicImage::from(img);
                        self.image_received.set(Arc::new(img));
                    }
                }

                for frame in frames.frames_of_type::<PoseFrame>() {
                    let quat = frame.rotation();
                    self.imu_received.set(IMUFrame {
                        acceleration: frame.acceleration().into(),
                        rotation: to_euler_angles(RotationType::Intrinsic, RotationSequence::YXZ, (quat[0], [quat[1], quat[2], quat[3]])).into()
                    });
                }
            }
        })
        .await
    }
}

pub fn discover_all_realsense() -> anyhow::Result<impl Iterator<Item = RealSenseCamera>> {
    let context = Context::new()?;
    let devices = context.query_devices(HashSet::new());
    let context = Arc::new(Mutex::new(context));

    Ok(devices.into_iter().map(move |device| RealSenseCamera {
        device,
        context: context.clone(),
        image_received: Default::default(),
        imu_received: Default::default(),
    }))
}
