use crate::metadata::MetaManager;
use risingwave_pb::metadata::id_generator_service_server::IdGeneratorService;
use risingwave_pb::metadata::{GetIdRequest, GetIdResponse};
use std::sync::Arc;
use tonic::{Request, Response, Status};

#[derive(Clone)]
pub struct IdGeneratorServiceImpl {
    mmc: Arc<MetaManager>,
}

impl IdGeneratorServiceImpl {
    pub fn new(mmc: Arc<MetaManager>) -> Self {
        IdGeneratorServiceImpl { mmc }
    }
}

#[async_trait::async_trait]
impl IdGeneratorService for IdGeneratorServiceImpl {
    #[cfg(not(tarpaulin_include))]
    async fn get_id(
        &self,
        request: Request<GetIdRequest>,
    ) -> Result<Response<GetIdResponse>, Status> {
        let _req = request.into_inner();
        Ok(Response::new(GetIdResponse {
            id: self
                .mmc
                .id_generator
                .generate()
                .await
                .map_err(|e| e.to_grpc_status())?,
        }))
    }
}