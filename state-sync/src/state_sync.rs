// Copyright (c) The Diem Core Contributors
// SPDX-License-Identifier: Apache-2.0
use crate::{
    coordinator::{CoordinatorMessage, StateSyncCoordinator, SyncRequest},
    counters,
    executor_proxy::{ExecutorProxy, ExecutorProxyTrait},
    network::{StateSyncEvents, StateSyncSender},
};
use anyhow::{format_err, Result};
use diem_config::{
    config::{NodeConfig, RoleType, StateSyncConfig, UpstreamConfig},
    network_id::NodeNetworkId,
};
use diem_mempool::{CommitNotification, CommitResponse};
use diem_types::{
    contract_event::ContractEvent, epoch_change::Verifier, epoch_state::EpochState,
    ledger_info::LedgerInfoWithSignatures, transaction::Transaction, waypoint::Waypoint,
};
use executor_types::{ChunkExecutor, ExecutedTrees};
use futures::{
    channel::{mpsc, oneshot},
    future::Future,
    SinkExt,
};
use std::{
    boxed::Box,
    collections::HashMap,
    sync::Arc,
    time::{Duration, SystemTime},
};
use storage_interface::DbReader;
use subscription_service::ReconfigSubscription;
use tokio::{
    runtime::{Builder, Runtime},
    time::timeout,
};

/// SyncingState contains the following fields:
/// * `committed_ledger_info` holds the latest certified ledger info (committed to storage),
///    i.e., the ledger info for the highest version for which storage has all ledger state.
/// * `synced_trees` holds the latest transaction accumulator and state tree (which may
///    or may not be committed to storage), i.e., some ledger state for the next highest
///    ledger info version is missing.
/// * `trusted_epoch_state` corresponds to the current epoch if the highest committed
///    ledger info (`committed_ledger_info`) is in the middle of the epoch, otherwise, it
///    corresponds to the next epoch if the highest committed ledger info ends the epoch.
///
/// Note: `committed_ledger_info` is used for helping other Diem nodes synchronize (i.e.,
/// it corresponds to the highest version we have a proof for in storage). `synced_trees`
/// is used locally for retrieving missing chunks for the local storage.
#[derive(Clone)]
pub struct SyncingState {
    committed_ledger_info: LedgerInfoWithSignatures,
    synced_trees: ExecutedTrees,
    trusted_epoch_state: EpochState,
}

impl SyncingState {
    pub fn new(
        committed_ledger_info: LedgerInfoWithSignatures,
        synced_trees: ExecutedTrees,
        current_epoch_state: EpochState,
    ) -> Self {
        let trusted_epoch_state = committed_ledger_info
            .ledger_info()
            .next_epoch_state()
            .cloned()
            .unwrap_or(current_epoch_state);

        SyncingState {
            committed_ledger_info,
            synced_trees,
            trusted_epoch_state,
        }
    }

    pub fn committed_epoch(&self) -> u64 {
        self.committed_ledger_info.ledger_info().epoch()
    }

    pub fn committed_ledger_info(&self) -> LedgerInfoWithSignatures {
        self.committed_ledger_info.clone()
    }

    pub fn committed_version(&self) -> u64 {
        self.committed_ledger_info.ledger_info().version()
    }

    /// Returns the highest available version in the local storage, even if it's not
    /// committed (i.e., covered by a ledger info).
    pub fn synced_version(&self) -> u64 {
        self.synced_trees.version().unwrap_or(0)
    }

    pub fn trusted_epoch(&self) -> u64 {
        self.trusted_epoch_state.epoch
    }

    pub fn verify_ledger_info(&self, ledger_info: &LedgerInfoWithSignatures) -> Result<()> {
        self.trusted_epoch_state.verify(ledger_info)
    }
}

pub struct StateSync {
    _runtime: Runtime,
    coordinator_sender: mpsc::UnboundedSender<CoordinatorMessage>,
}

impl StateSync {
    pub fn bootstrap(
        network: Vec<(NodeNetworkId, StateSyncSender, StateSyncEvents)>,
        state_sync_to_mempool_sender: mpsc::Sender<CommitNotification>,
        storage: Arc<dyn DbReader>,
        executor: Box<dyn ChunkExecutor>,
        config: &NodeConfig,
        waypoint: Waypoint,
        reconfig_event_subscriptions: Vec<ReconfigSubscription>,
    ) -> Self {
        let runtime = Builder::new()
            .thread_name("state-sync")
            .threaded_scheduler()
            .enable_all()
            .build()
            .expect("[state synchronizer] failed to create runtime");

        let executor_proxy = ExecutorProxy::new(storage, executor, reconfig_event_subscriptions);
        Self::bootstrap_with_executor_proxy(
            runtime,
            network,
            state_sync_to_mempool_sender,
            config.base.role,
            waypoint,
            &config.state_sync,
            config.upstream.clone(),
            executor_proxy,
        )
    }

    pub fn bootstrap_with_executor_proxy<E: ExecutorProxyTrait + 'static>(
        runtime: Runtime,
        network: Vec<(NodeNetworkId, StateSyncSender, StateSyncEvents)>,
        state_sync_to_mempool_sender: mpsc::Sender<CommitNotification>,
        role: RoleType,
        waypoint: Waypoint,
        state_sync_config: &StateSyncConfig,
        upstream_config: UpstreamConfig,
        executor_proxy: E,
    ) -> Self {
        let (coordinator_sender, coordinator_receiver) = mpsc::unbounded();

        let initial_state = executor_proxy
            .get_local_storage_state()
            .expect("[state sync] Start failure: cannot sync with storage.");

        let network_senders: HashMap<_, _> = network
            .iter()
            .map(|(network_id, sender, _events)| (network_id.clone(), sender.clone()))
            .collect();

        let coordinator = StateSyncCoordinator::new(
            coordinator_receiver,
            state_sync_to_mempool_sender,
            network_senders,
            role,
            waypoint,
            state_sync_config.clone(),
            upstream_config,
            executor_proxy,
            initial_state,
        )
        .expect("Unable to create sync coordinator");
        runtime.spawn(coordinator.start(network));

        Self {
            _runtime: runtime,
            coordinator_sender,
        }
    }

    pub fn create_client(&self) -> StateSyncClient {
        StateSyncClient::new(self.coordinator_sender.clone())
    }

    /// The function returns a future that is fulfilled when the state synchronizer is
    /// caught up with the waypoint specified in the local config.
    pub async fn wait_until_initialized(&self) -> Result<()> {
        let mut sender = self.coordinator_sender.clone();
        let (cb_sender, cb_receiver) = oneshot::channel();
        sender
            .send(CoordinatorMessage::WaitInitialize(cb_sender))
            .await?;
        cb_receiver.await?
    }
}

pub struct StateSyncClient {
    coordinator_sender: mpsc::UnboundedSender<CoordinatorMessage>,
}

impl StateSyncClient {
    pub fn new(coordinator_sender: mpsc::UnboundedSender<CoordinatorMessage>) -> Self {
        Self { coordinator_sender }
    }

    /// Sync validator's state to target.
    /// In case of success (`Result::Ok`) the LI of storage is at the given target.
    /// In case of failure (`Result::Error`) the LI of storage remains unchanged, and the validator
    /// can assume there were no modifications to the storage made.
    /// It is up to state synchronizer to decide about the specific criteria for the failure
    /// (e.g., lack of progress with all of the peer validators).
    pub fn sync_to(&self, target: LedgerInfoWithSignatures) -> impl Future<Output = Result<()>> {
        let mut sender = self.coordinator_sender.clone();
        let (callback, cb_receiver) = oneshot::channel();
        let request = SyncRequest {
            callback,
            target,
            last_progress_tst: SystemTime::now(),
        };
        async move {
            sender
                .send(CoordinatorMessage::Request(Box::new(request)))
                .await?;
            cb_receiver.await?
        }
    }

    /// Notifies state synchronizer about newly committed transactions.
    pub fn commit(
        &self,
        committed_txns: Vec<Transaction>,
        reconfig_events: Vec<ContractEvent>,
    ) -> impl Future<Output = Result<()>> {
        let mut sender = self.coordinator_sender.clone();
        async move {
            let (callback, callback_rcv) = oneshot::channel();
            sender
                .send(CoordinatorMessage::Commit(
                    committed_txns,
                    reconfig_events,
                    callback,
                ))
                .await?;

            match timeout(Duration::from_secs(5), callback_rcv).await {
                Err(_) => {
                    counters::COMMIT_FLOW_FAIL
                        .with_label_values(&[counters::STATE_SYNC_LABEL])
                        .inc();
                    Err(format_err!("[state sync client] failed to receive commit ACK from state synchronizer on time"))
                }
                Ok(resp) => {
                    let CommitResponse { msg } = resp??;
                    if msg != "" {
                        Err(format_err!("[state sync client] commit failed: {:?}", msg))
                    } else {
                        Ok(())
                    }
                }
            }
        }
    }

    /// Returns information about StateSynchronizer internal state. This should only
    /// be used by tests.
    #[cfg(test)]
    pub fn get_state(&self) -> impl Future<Output = Result<SyncingState>> {
        let mut sender = self.coordinator_sender.clone();
        let (cb_sender, cb_receiver) = oneshot::channel();
        async move {
            sender.send(CoordinatorMessage::GetState(cb_sender)).await?;
            let info = cb_receiver.await?;
            Ok(info)
        }
    }
}
