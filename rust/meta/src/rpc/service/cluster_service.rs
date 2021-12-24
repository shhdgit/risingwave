use std::sync::Arc;

use risingwave_common::array::InternalError;
use risingwave_common::error::ErrorCode::ProtocolError;
use risingwave_common::error::RwError;
use risingwave_pb::meta::cluster_service_server::ClusterService;
use risingwave_pb::meta::{
    AddWorkerNodeRequest, AddWorkerNodeResponse, ClusterType, DeleteWorkerNodeRequest,
    DeleteWorkerNodeResponse, ListAllNodesRequest, ListAllNodesResponse,
};
use tonic::{Request, Response, Status};

use crate::cluster::WorkerNodeMetaManager;
use crate::manager::MetaManager;

#[derive(Clone)]
pub struct ClusterServiceImpl {
    mmc: Arc<MetaManager>,
}

impl ClusterServiceImpl {
    pub fn new(mmc: Arc<MetaManager>) -> Self {
        ClusterServiceImpl { mmc }
    }
}

#[async_trait::async_trait]
impl ClusterService for ClusterServiceImpl {
    async fn add_worker_node(
        &self,
        request: Request<AddWorkerNodeRequest>,
    ) -> Result<Response<AddWorkerNodeResponse>, Status> {
        let req = request.into_inner();
        let cluster_type = match req.cluster_type {
            0 => ClusterType::Olap,
            1 => ClusterType::Streaming,
            _ => ClusterType::Unknown,
        };
        if let Some(host) = req.host {
            let worker_node_res = self.mmc.add_worker_node(host, cluster_type).await;
            match worker_node_res {
                Ok(worker_node) => Ok(Response::new(AddWorkerNodeResponse {
                    status: None,
                    node: Some(worker_node),
                })),
                Err(_e) => Err(RwError::from(InternalError(
                    "worker node already exists".to_string(),
                ))
                .to_grpc_status()),
            }
        } else {
            Err(RwError::from(ProtocolError("host address invalid".to_string())).to_grpc_status())
        }
    }

    async fn delete_worker_node(
        &self,
        request: Request<DeleteWorkerNodeRequest>,
    ) -> Result<Response<DeleteWorkerNodeResponse>, Status> {
        let req = request.into_inner();
        let cluster_type = match req.cluster_type {
            0 => ClusterType::Olap,
            1 => ClusterType::Streaming,
            _ => ClusterType::Unknown,
        };
        if let Some(node) = req.node {
            let delete_res = self.mmc.delete_worker_node(node, cluster_type).await;
            match delete_res {
                Ok(()) => Ok(Response::new(DeleteWorkerNodeResponse { status: None })),
                Err(_e) => Err(
                    RwError::from(InternalError("worker node not exists".to_string()))
                        .to_grpc_status(),
                ),
            }
        } else {
            Err(RwError::from(ProtocolError("work node invalid".to_string())).to_grpc_status())
        }
    }

    async fn list_all_nodes(
        &self,
        request: Request<ListAllNodesRequest>,
    ) -> Result<Response<ListAllNodesResponse>, Status> {
        let req = request.into_inner();
        let cluster_type = match req.cluster_type {
            0 => ClusterType::Olap,
            1 => ClusterType::Streaming,
            _ => ClusterType::Unknown,
        };
        let node_list = self.mmc.list_worker_node(cluster_type).await.unwrap();
        Ok(Response::new(ListAllNodesResponse {
            status: None,
            nodes: node_list,
        }))
    }
}