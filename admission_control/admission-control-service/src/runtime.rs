// Copyright (c) The Libra Core Contributors
// SPDX-License-Identifier: Apache-2.0

use crate::{admission_control_service::AdmissionControlService, upstream_proxy::UpstreamProxy};
use admission_control_proto::proto::admission_control::{
    create_admission_control, AdmissionControlClient, SubmitTransactionRequest,
    SubmitTransactionResponse,
};
use config::config::NodeConfig;
use futures::{
    channel::{mpsc, oneshot},
    future::{FutureExt, TryFutureExt},
};
use grpc_helpers::ServerHandle;
use grpcio::{ChannelBuilder, EnvBuilder, ServerBuilder};
use libra_mempool::proto::mempool::MempoolClient;
use network::validator_network::{AdmissionControlNetworkEvents, AdmissionControlNetworkSender};
use std::{cmp::min, sync::Arc};
use storage_client::{StorageRead, StorageReadServiceClient};
use tokio::runtime::{Builder, Runtime};
use vm_validator::vm_validator::VMValidator;

/// Handle for AdmissionControl Runtime
pub struct AdmissionControlRuntime {
    /// gRPC server to serve request between client and AC
    _grpc_server: ServerHandle,
    /// separate AC runtime
    _upstream_proxy: Runtime,
}

impl AdmissionControlRuntime {
    /// setup Admission Control runtime
    pub fn bootstrap(
        config: &NodeConfig,
        network_sender: AdmissionControlNetworkSender,
        network_events: Vec<AdmissionControlNetworkEvents>,
    ) -> Self {
        let (upstream_proxy_sender, upstream_proxy_receiver) = mpsc::unbounded();

        let (grpc_server, client) = Self::setup_ac(&config, upstream_proxy_sender);

        let upstream_proxy_runtime = Builder::new()
            .name_prefix("ac-upstream-proxy-")
            .build()
            .expect("[admission control] failed to create runtime");

        let executor = upstream_proxy_runtime.executor();

        let upstream_proxy =
            UpstreamProxy::new(config, network_sender, upstream_proxy_receiver, client);

        executor.spawn(
            upstream_proxy
                .process_network_messages(network_events)
                .boxed()
                .unit_error()
                .compat(),
        );

        Self {
            _grpc_server: ServerHandle::setup(grpc_server),
            _upstream_proxy: upstream_proxy_runtime,
        }
    }

    /// setup Admission Control gRPC service
    pub fn setup_ac(
        config: &NodeConfig,
        upstream_proxy_sender: mpsc::UnboundedSender<(
            SubmitTransactionRequest,
            oneshot::Sender<failure::Result<SubmitTransactionResponse>>,
        )>,
    ) -> (::grpcio::Server, AdmissionControlClient) {
        let env = Arc::new(
            EnvBuilder::new()
                .name_prefix("grpc-ac-")
                .cq_count(min(num_cpus::get() * 2, 32))
                .build(),
        );
        let port = config.admission_control.admission_control_service_port;

        // Create mempool client if the node is validator.
        let connection_str = format!("localhost:{}", config.mempool.mempool_service_port);
        let env2 = Arc::new(EnvBuilder::new().name_prefix("grpc-ac-mem-").build());
        let mempool_client = if config.is_validator() {
            Some(Arc::new(MempoolClient::new(
                ChannelBuilder::new(env2).connect(&connection_str),
            )))
        } else {
            None
        };

        // Create storage read client
        let storage_client: Arc<dyn StorageRead> = Arc::new(StorageReadServiceClient::new(
            Arc::new(EnvBuilder::new().name_prefix("grpc-ac-sto-").build()),
            "localhost",
            config.storage.port,
        ));

        let vm_validator = Arc::new(VMValidator::new(&config, Arc::clone(&storage_client)));

        let handle = AdmissionControlService::new(
            mempool_client,
            storage_client,
            vm_validator,
            config
                .admission_control
                .need_to_check_mempool_before_validation,
            upstream_proxy_sender,
        );
        let service = create_admission_control(handle);
        let server = ServerBuilder::new(Arc::clone(&env))
            .register_service(service)
            .bind(config.admission_control.address.clone(), port)
            .build()
            .expect("Unable to create grpc server");

        let connection_str = format!("localhost:{}", port);
        let client = AdmissionControlClient::new(ChannelBuilder::new(env).connect(&connection_str));
        (server, client)
    }
}
