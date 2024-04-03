#![feature(new_uninit, ptr_metadata, alloc_layout_extra, convert_float_to_int)]

use std::{convert::FloatToInt, ops::Mul, sync::{atomic::{AtomicUsize, Ordering}, Arc}};

use dst_init::{dst, BoxExt, Slice, SliceExt};
use nalgebra::{Isometry3, Point2, Point3, RealField, UnitQuaternion};
use quadtree_rs::{area::AreaBuilder, Quadtree};
use rig::RobotElementRef;
use simba::scalar::SubsetOf;
use unros::{anyhow, async_trait, pubsub::{subs::Subscription, Publisher, PublisherRef, Subscriber}, setup_logging, Node, NodeIntrinsics, RuntimeContext};


#[derive(Clone, Copy)]
struct HeightCell<N> {
    total_height: N,
    count: usize,
}


#[derive(Clone)]
pub struct Points<T> {
    pub points: T,
    pub robot_element: RobotElementRef,
}


struct CostmapFrame<N> {
    quadtree: Quadtree<usize, HeightCell<N>>,
    max_density: usize,
    max_height: N,
    min_height: N,
    resolution: N,
    isometry: Isometry3<N>
}


#[dst]
struct CostmapInner<N> {
    point_count: usize,
    threshold: N,
    frames: [Arc<CostmapFrame<N>>]
}


#[derive(Clone)]
pub struct Costmap<N=f64> {
    inner: Arc<CostmapInner<N>>
}

impl<N: RealField> Costmap<N> {
    pub fn is_global_point_safe(&self, point: Point3<N>, radius: N) -> bool {
        false
    }
}


pub struct CostmapGenerator<N: RealField=f32> {
    pub window_length: usize,
    octree_sub: Subscriber<CostmapFrame<N>>,
    intrinsics: NodeIntrinsics<Self>,
    costmap_pub: Publisher<Costmap<N>>
}


pub trait PointTransformer<N: RealField> {
    fn transform(point: Point3<N>, robot_element: &RobotElementRef) -> Point3<N>;
}


pub struct AddRotation(());
pub struct NoTransform(());


impl<N: RealField> PointTransformer<N> for AddRotation
where f32: SubsetOf<N>
{
    fn transform(point: Point3<N>, robot_element: &RobotElementRef) -> Point3<N> {
        let rot: UnitQuaternion<N> = nalgebra::convert(robot_element.get_global_isometry().rotation);
        rot.transform_point(&point)
    }
}


impl<N: RealField> PointTransformer<N> for NoTransform {
    fn transform(point: Point3<N>, _robot_element: &RobotElementRef) -> Point3<N> {
        point
    }
}


impl<N: RealField + FloatToInt<isize> + Copy> CostmapGenerator<N> {
    pub fn create_points_sub<T, F>(&self, resolution: N) -> impl Subscription<Item=Points<T>>
    where
        T: IntoIterator<Item = Point3<N>>,
        F: PointTransformer<N>,
        f32: SubsetOf<N>
    {
        assert!(resolution != N::zero());
        self.octree_sub.create_subscription().filter_map(move |original_points: Points<T>| {
            let points: Box<[_]> = original_points.points.into_iter().map(|mut p| {
                p = F::transform(p, &original_points.robot_element);
                let pt = unsafe {
                    Point2::<isize>::new((p.x / resolution).round().to_int_unchecked(), (p.z / resolution).round().to_int_unchecked())
                };
                (pt, p.y)
            }).collect();

            let mut points_iter = points.iter().copied();
            let Some((first_point, first_height)) = points_iter.next() else {
                return None;
            };
            let mut min_x = first_point.x;
            let mut min_y = first_point.y;
            let mut min_height = first_height;
            let mut max_x = first_point.x;
            let mut max_y = first_point.y;
            let mut max_height = first_height;

            for (point, height) in points_iter {
                if point.x < min_x {
                    min_x = point.x;
                } else if point.x > max_x {
                    max_x = point.x;
                }
                if point.y < min_y {
                    min_y = point.y;
                } else if point.y > max_y {
                    max_y = point.y;
                }
                if height < min_height {
                    min_height = height;
                } else if height > max_height {
                    max_height = height;
                }
            }

            let max_range = (max_x - min_x).max(max_y - min_y) as usize;
            let depth = max_range.ilog2().next_power_of_two() as usize;
            let mut max_density = 0;

            let mut quadtree = Quadtree::<usize, HeightCell<N>>::new(depth);

            for (point, height) in points.iter().copied() {
                let mut modified_count = AtomicUsize::default();

                let anchor = quadtree_rs::point::Point { x: (point.x - min_x) as usize, y: (point.y - min_y) as usize  };
                quadtree.modify(
                    AreaBuilder::default()
                        .anchor(anchor)
                        .dimensions((1, 1))
                        .build()
                        .unwrap(),
                    |pt| {
                        pt.count += 1;
                        modified_count.store(pt.count, Ordering::Relaxed);
                        pt.total_height += height;
                    },
                );

                if *modified_count.get_mut() == 0 {
                    quadtree.insert_pt(
                        anchor,
                        HeightCell {
                            total_height: height,
                            count: 1,
                        },
                    );
                    max_density = max_density.max(1);
                } else {
                    max_density = max_density.max(*modified_count.get_mut());
                }
            }

            Some(CostmapFrame {
                quadtree,
                max_density,
                max_height,
                min_height,
                resolution,
                isometry: nalgebra::convert(original_points.robot_element.get_global_isometry())
            })
        })
    }

    pub fn get_costmap_pub(&self) -> PublisherRef<Costmap<N>> {
        self.costmap_pub.get_ref()
    }
}


#[async_trait]
impl<N: RealField> Node for CostmapGenerator<N> {
    const DEFAULT_NAME: &'static str = "costmap-generator";

    async fn run(self, context: RuntimeContext) -> anyhow::Result<()> {
        setup_logging!(context);
        let mut costmap_frames: Box<[Arc<CostmapFrame<N>>]> = std::iter::repeat_with(|| Arc::new(CostmapFrame {
            quadtree: Quadtree::new(0),
            max_density: 0,
            max_height: N::zero(),
            min_height: N::zero(),
            resolution: N::one(),
            isometry: nalgebra::Isometry3::identity()
        }).into()).take(self.window_length).collect();
        let mut frame_index = 0usize;
        
        loop {
            let inner = CostmapInnerInit { point_count: costmap_frames.iter().map(|x| x.quadtree.len()).sum(), frames: Slice::iter_init(costmap_frames.len(), costmap_frames.iter().cloned()), };
            let inner = Box::emplace(inner);
            self.costmap_pub.set(Costmap { inner: inner.into() });

            let Some(frame) = self.octree_sub.recv_or_closed().await else {
                break Ok(());
            };
            costmap_frames[frame_index] = Arc::new(frame);
            frame_index += 1;
            if frame_index >= self.window_length {
                frame_index = 0;
            }
        }
    }

    fn get_intrinsics(&mut self) -> &mut NodeIntrinsics<Self> {
        &mut self.intrinsics
    }
}