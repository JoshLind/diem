// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0

#![forbid(unsafe_code)]

//! Used to perform catching up between nodes for committed states.
//! Used for node restarts, network partitions, full node syncs
#![recursion_limit = "1024"]

pub use self::state_sync::{StateSynchronizer, StateSynchronizerClient};

pub mod chunk_request;
pub mod chunk_response;
pub mod coordinator;
mod counters;
mod executor_proxy;
mod logging;
pub mod network;
mod request_manager;
mod state_sync;

#[cfg(any(feature = "fuzzing", test))]
mod tests;
#[cfg(any(feature = "fuzzing", test))]
pub use tests::fuzzing;
