use std::{
    net::SocketAddrV4,
    sync::{atomic::{AtomicBool, Ordering}, Arc},
    time::{Duration, Instant},
};

use image::DynamicImage;
use lunabot::{make_negotiation, ControlsPacket, ImportantMessage, VIDEO_HEIGHT, VIDEO_WIDTH};
use navigator::drive::Steering;
use networking::{
    negotiation::{ChannelNegotiation, Negotiation},
    new_client, ConnectionError, NetworkConnector, NetworkNode,
};
use ordered_float::NotNan;
use spin_sleep::SpinSleeper;
use unros::{
    anyhow, async_trait, asyncify_run,
    logging::{
        dump::{ScalingFilter, VideoDataDump},
        get_log_pub,
    },
    pubsub::{subs::DirectSubscription, MonoPublisher, Publisher, PublisherRef, Subscriber},
    setup_logging, tokio, DropCheck, Node, NodeIntrinsics, RuntimeContext,
};

/// A remote connection to `Lunabase`
pub struct Telemetry {
    network_node: NetworkNode,
    network_connector: NetworkConnector,
    pub server_addr: SocketAddrV4,
    pub camera_delta: Duration,
    steering_signal: Publisher<Steering>,
    image_subscriptions: Subscriber<Arc<DynamicImage>>,
    intrinsics: NodeIntrinsics<Self>,
    negotiation: Negotiation<(
        ChannelNegotiation<ImportantMessage>,
        ChannelNegotiation<Arc<str>>,
        ChannelNegotiation<u8>,
        ChannelNegotiation<ControlsPacket>,
        ChannelNegotiation<Arc<str>>,
    )>,
    video_addr: SocketAddrV4,
    cam_width: u32,
    cam_height: u32,
    cam_fps: usize,
}

impl Telemetry {
    pub async fn new(
        server_addr: impl Into<SocketAddrV4>,
        cam_width: u32,
        cam_height: u32,
        cam_fps: usize,
    ) -> anyhow::Result<Self> {
        let server_addr = server_addr.into();
        let mut video_addr = server_addr;
        video_addr.set_port(video_addr.port() + 1);

        let (network_node, network_connector) = new_client()?;

        Ok(Self {
            network_node,
            network_connector,
            server_addr,
            steering_signal: Publisher::default(),
            image_subscriptions: Subscriber::new(1),
            camera_delta: Duration::from_millis((1000 / cam_fps) as u64),
            intrinsics: Default::default(),
            negotiation: make_negotiation(),
            cam_width,
            cam_height,
            video_addr,
            cam_fps
        })
    }

    pub fn steering_pub(&self) -> PublisherRef<Steering> {
        self.steering_signal.get_ref()
    }

    pub fn create_image_subscription(&self) -> DirectSubscription<Arc<DynamicImage>> {
        self.image_subscriptions.create_subscription()
    }
}

#[async_trait]
impl Node for Telemetry {
    const DEFAULT_NAME: &'static str = "telemetry";

    fn get_intrinsics(&mut self) -> &mut NodeIntrinsics<Self> {
        &mut self.intrinsics
    }

    async fn run(mut self, context: RuntimeContext) -> anyhow::Result<()> {
        self.network_node
            .get_intrinsics()
            .manually_run(context.get_name().clone());

        let mut video_dump = VideoDataDump::new_rtp(
            self.cam_width,
            self.cam_height,
            VIDEO_WIDTH,
            VIDEO_HEIGHT,
            ScalingFilter::FastBilinear,
            self.video_addr,
            self.cam_fps,
        )?;

        let sdp: Arc<str> = Arc::from(video_dump.generate_sdp().unwrap().into_boxed_str());
        let enable_camera = Arc::new(AtomicBool::new(true));
        let enable_camera2 = enable_camera.clone();

        let drop_check = DropCheck::default();
        let drop_observe = drop_check.get_observing();
        let context2 = context.clone();

        let cam_fut = asyncify_run(move || {
            setup_logging!(context2);
            let sleeper = SpinSleeper::default();

            loop {
                let mut start_service = Instant::now();
                loop {
                    if drop_observe.has_dropped() {
                        return Ok(());
                    }
                    if !enable_camera.load(Ordering::Relaxed) {
                        drop(video_dump);
                        break;
                    }
                    if let Some(img) = self.image_subscriptions.try_recv() {
                        video_dump.write_frame(img.clone())?;
                    }
    
                    let elapsed = start_service.elapsed();
                    start_service += elapsed;
                    sleeper.sleep(self.camera_delta.saturating_sub(elapsed));
                }
                loop {
                    if drop_observe.has_dropped() {
                        return Ok(());
                    }
                    if enable_camera.load(Ordering::Relaxed) {
                        loop {
                            match VideoDataDump::new_rtp(
                                self.cam_width,
                                self.cam_height,
                                VIDEO_WIDTH,
                                VIDEO_HEIGHT,
                                ScalingFilter::FastBilinear,
                                self.video_addr,
                                self.cam_fps,
                            ) {
                                Ok(x) => {
                                    video_dump = x;
                                    break;
                                }
                                Err(e) => error!("Failed to create video dump: {e}")
                            }
                            let start_service = Instant::now();
                            while start_service.elapsed().as_millis() < 2000 {
                                if drop_observe.has_dropped() {
                                    return Ok(());
                                }
                                sleeper.sleep(self.camera_delta);
                            }
                        }
                        break;
                    }
                    sleeper.sleep(self.camera_delta);
                }
            }
        });
        let enable_camera = enable_camera2;

        let context2 = context.clone();
        setup_logging!(context2);

        let peer_fut = async {
            loop {
                info!("Connecting to lunabase...");
                let peer = loop {
                    match self
                        .network_connector
                        .connect_to(self.server_addr.into(), &12u8)
                        .await
                    {
                        Ok(x) => break x,
                        Err(ConnectionError::ServerDropped) => return Ok(()),
                        Err(ConnectionError::Timeout) => {}
                    };
                };
                let (important, camera, _odometry, controls, logs) =
                    match peer.negotiate(&self.negotiation).await {
                        Ok(x) => x,
                        Err(e) => {
                            error!("Failed to negotiate with lunabase!: {e:?}");
                            continue;
                        }
                    };
                info!("Connected to lunabase!");
                get_log_pub().accept_subscription(logs.create_reliable_subscription());

                let important_fut = async {
                    let mut _important_pub = MonoPublisher::from(important.create_reliable_subscription());
                    let important_sub = Subscriber::new(8);
                    important.accept_subscription(important_sub.create_subscription());

                    loop {
                        let Some(result) = important_sub.recv_or_closed().await else {
                            break;
                        };
                        let msg = match result {
                            Ok(x) => x,
                            Err(e) => {
                                error!("Error receiving important msg: {e}");
                                continue;
                            }
                        };
                        match msg {
                            ImportantMessage::EnableCamera => enable_camera.store(true, Ordering::Relaxed),
                            ImportantMessage::DisableCamera => enable_camera.store(false, Ordering::Relaxed),
                        }
                    }
                };

                let steering_fut = async {
                    let mut controls_pub = MonoPublisher::from(controls.create_unreliable_subscription());
                    let controls_sub = Subscriber::new(1);
                    controls.accept_subscription(controls_sub.create_subscription());

                    loop {
                        let Some(result) = controls_sub.recv_or_closed().await else {
                            break;
                        };
                        let controls = match result {
                            Ok(x) => x,
                            Err(e) => {
                                error!("Error receiving steering: {e}");
                                continue;
                            }
                        };
                        controls_pub.set(controls);
                        self.steering_signal.set(Steering::from_drive_and_steering(
                            NotNan::new(controls.drive as f32 / 127.0).unwrap(),
                            NotNan::new(controls.steering as f32 / 127.0).unwrap(),
                        ));
                    }
                };

                let camera_fut = async {
                    let camera_pub = Publisher::default();
                    let camera_sub = Subscriber::new(1);
                    camera.accept_subscription(camera_sub.create_subscription());
                    camera_pub.accept_subscription(camera.create_reliable_subscription());
                    camera_pub.set(sdp.clone());

                    loop {
                        let Some(result) = camera_sub.recv_or_closed().await else {
                            break;
                        };
                        let _ = match result {
                            Ok(x) => x,
                            Err(e) => {
                                error!("Error receiving camera msg: {e}");
                                continue;
                            }
                        };

                        info!("Resending SDP");
                        camera_pub.set(sdp.clone());
                    }
                };

                tokio::select! {
                    _ = steering_fut => {}
                    _ = camera_fut => {}
                    _ = important_fut => {}
                }
                error!("Disconnected from lunabase!");
            }
        };

        tokio::select! {
            res = cam_fut => res,
            res = peer_fut => res,
            res = self.network_node.run(context) => res,
        }
    }
}
