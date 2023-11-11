use std::ops::Deref;

use async_trait::async_trait;
use tokio::sync::watch;

#[async_trait]
pub trait WatchTrait<T>: Send + Sync + 'static {
    async fn get(&mut self) -> T;
    async fn wait_for_change(&mut self) -> T;

    fn try_get(&mut self) -> Option<T>;
}


struct MappedWatched<T, S> {
    recv: Option<Box<dyn WatchTrait<S>>>,
    mapper: Box<dyn FnMut(S) -> T + Send + Sync>
}


#[async_trait]
impl<T: 'static, S: 'static> WatchTrait<T> for MappedWatched<T, S> {
    async fn get(&mut self) -> T {
        if let Some(recv) = &mut self.recv {
            (self.mapper)(recv.get().await)
        } else {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    async fn wait_for_change(&mut self) -> T {
        if let Some(recv) = &mut self.recv {
            (self.mapper)(recv.wait_for_change().await)
        } else {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    fn try_get(&mut self) -> Option<T> {
        self.recv.as_mut().and_then(|x| x.try_get()).map(|x| (self.mapper)(x))
    }
}


pub struct WatchedSubscription<T> {
    pub(super) recv: Option<Box<dyn WatchTrait<T>>>
}

static_assertions::assert_impl_all!(WatchedSubscription<()>: Send, Sync);

impl<T: 'static> WatchedSubscription<T> {
    pub fn none() -> Self {
        Self {
            recv: None
        }
    }

    pub async fn get(&mut self) -> T {
        if let Some(recv) = &mut self.recv {
            recv.get().await
        } else {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    pub async fn wait_for_change(&mut self) -> T {
        if let Some(recv) = &mut self.recv {
            recv.get().await
        } else {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }

    pub fn try_get(&mut self) -> Option<T> {
        self.recv.as_mut().and_then(|x| x.try_get())
    }

    pub fn map<V: 'static>(self, mapper: impl FnMut(T) -> V + 'static + Send + Sync) -> WatchedSubscription<V> {
        WatchedSubscription {
            recv: Some(Box::new(MappedWatched {
                            recv: self.recv,
                            mapper: Box::new(mapper)
                        }))
        }
    }
}


#[async_trait]
impl<T: Clone + Send + Sync + 'static> WatchTrait<T> for watch::Receiver<Option<T>> {
    async fn get(&mut self) -> T {
        if let Some(x) = self.borrow_and_update().deref() {
            return x.clone();
        }
        if self.changed().await.is_err() {
            std::future::pending::<()>().await;
            unreachable!()
        } else {
            self.borrow().as_ref().unwrap().clone()
        }
    }

    async fn wait_for_change(&mut self) -> T {
        if self.changed().await.is_err() {
            std::future::pending::<()>().await;
            unreachable!()
        } else {
            self.borrow().as_ref().unwrap().clone()
        }
    }

    fn try_get(&mut self) -> Option<T> {
        self.borrow_and_update().as_ref().map(Clone::clone)
    }
}
