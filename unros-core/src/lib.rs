//! Unros is an experimental alternative to the ROS 1 & 2 frameworks.
//!
//! It is written from the ground up in Rust and seeks to replicate most
//! of the common functionality in ROS while adding some extra features
//! that exploit Rust's abilities.
//!
//! This crate contains the core functionality which defines what this
//! framework offers:
//!
//! 1. The Node trait
//! 2. A complete logging system
//! 3. An asynchronous Node runtime
//! 4. Publisher and Subscribers (analagous to ROS publisher and subscribers)
//! 5. The Service framework (analagous to ROS actions and services)

#![allow(clippy::type_complexity)]
#![feature(once_cell_try)]
#![feature(ptr_metadata)]
#![feature(alloc_layout_extra)]

use std::{borrow::Cow, marker::PhantomData, path::Path, sync::OnceLock};

pub mod logging;
pub mod node;
pub mod pubsub;
pub mod rng;
pub mod runtime;
pub mod service;
pub mod utils;

pub use anyhow;
use config::Config;
pub use log;
pub use rand;
pub use rayon;
use serde::Deserialize;
pub use tokio;

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Deserialize environment variables and the default config file into the given generic type.
pub fn get_env<'de, T: Deserialize<'de>>() -> anyhow::Result<T> {
    let mut config = Config::builder().add_source(config::Environment::with_prefix(""));
    if Path::new("settings.toml").exists() {
        config = config.add_source(config::File::with_name("settings.toml"));
    }
    if Path::new(".env").exists() {
        config = config.add_source(config::File::with_name(".env"));
    }
    CONFIG
        .get_or_try_init(|| config.build())?
        .clone()
        .try_deserialize()
        .map_err(Into::into)
}

#[derive(Clone, Debug)]
pub struct DontDrop<T: ShouldNotDrop + ?Sized> {
    pub name: Cow<'static, str>,
    pub ignore_drop: bool,
    phantom: PhantomData<T>,
}

impl<T: ShouldNotDrop + ?Sized> DontDrop<T> {
    pub fn new(name: impl Into<Cow<'static, str>>) -> Self {
        Self {
            name: name.into(),
            ignore_drop: false,
            phantom: PhantomData,
        }
    }

    pub fn remap<T2: ShouldNotDrop + ?Sized>(mut self) -> DontDrop<T2> {
        let ignore_drop = self.ignore_drop;
        self.ignore_drop = true;
        DontDrop {
            name: std::mem::take(&mut self.name),
            ignore_drop,
            phantom: PhantomData,
        }
    }
}

impl<T: ShouldNotDrop + ?Sized> Drop for DontDrop<T> {
    fn drop(&mut self) {
        if !self.ignore_drop {
            log::warn!("{} was dropped", self.name);
        }
    }
}

pub trait ShouldNotDrop {
    fn get_dont_drop(&mut self) -> &mut DontDrop<Self>;

    fn ignore_drop(&mut self) {
        self.get_dont_drop().ignore_drop = true;
    }
}
