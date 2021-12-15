use std::fmt::{Debug, Formatter};

use prost::Message;
use risingwave_common::array::DataChunk;
use risingwave_common::catalog::{Field, Schema, TableId};
use risingwave_common::error::ErrorCode::{InternalError, ProstError};
use risingwave_common::error::{Result, RwError};
use risingwave_pb::plan::StreamScanNode;
use risingwave_source::{BatchSourceReader, HighLevelKafkaSourceReaderContext, Source, SourceImpl};

use super::{BoxedExecutor, BoxedExecutorBuilder};
use crate::executor::{Executor, ExecutorBuilder};

pub struct StreamScanExecutor {
    reader: Box<dyn BatchSourceReader>,
    done: bool,
    schema: Schema,
}

impl BoxedExecutorBuilder for StreamScanExecutor {
    /// This function is designed for OLAP to initialize the `StreamScanExecutor`
    /// Things needed for initialization is
    /// 1. `StreamScanNode` whose definition can be shared by OLAP and Streaming
    /// 2. `SourceManager` whose definition can also be shared. But is it physically shared?
    fn new_boxed_executor(source: &ExecutorBuilder) -> Result<BoxedExecutor> {
        let stream_scan_node = StreamScanNode::decode(&(source.plan_node()).get_body().value[..])
            .map_err(ProstError)?;

        let table_id = TableId::from(&stream_scan_node.table_ref_id);

        let source_desc = source
            .global_task_env()
            .source_manager()
            .get_source(&table_id)?;

        let column_ids = stream_scan_node.get_column_ids();

        let fields = column_ids
            .iter()
            .map(|id| {
                source_desc
                    .columns
                    .iter()
                    .find(|c| c.column_id == *id)
                    .map(|col| Field {
                        data_type: col.data_type.clone(),
                    })
                    .ok_or_else(|| {
                        RwError::from(InternalError(format!(
                            "Failed to find column id: {} in source: {:?}",
                            id, table_id
                        )))
                    })
            })
            .collect::<Result<Vec<Field>>>()?;

        let reader: Box<dyn BatchSourceReader> = match source_desc.source.as_ref() {
            SourceImpl::HighLevelKafka(k) => Box::new(k.batch_reader(
                HighLevelKafkaSourceReaderContext {
                    query_id: Some(source.task_id.clone().query_id),
                    bound_timestamp_ms: Some(stream_scan_node.timestamp_ms),
                },
                column_ids.clone(),
            )?),
            SourceImpl::Table(_) => panic!("use table_scan to scan a table"),
        };

        Ok(Box::new(Self {
            reader,
            done: false,
            schema: Schema { fields },
        }))
    }
}

#[async_trait::async_trait]
impl Executor for StreamScanExecutor {
    async fn open(&mut self) -> Result<()> {
        self.reader.open().await
    }

    async fn next(&mut self) -> Result<Option<DataChunk>> {
        self.reader.next().await
    }

    async fn close(&mut self) -> Result<()> {
        self.reader.close().await
    }

    fn schema(&self) -> &Schema {
        &self.schema
    }
}

impl Debug for StreamScanExecutor {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StreamScanExecutor")
            .field("schema", &self.schema)
            .field("done", &self.done)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use itertools::Itertools;
    use risingwave_common::array::column::Column;
    use risingwave_common::array::{Array, I32Array};
    use risingwave_common::types::Int32Type;

    use super::*;

    struct MockSourceReader {}

    #[async_trait]
    impl BatchSourceReader for MockSourceReader {
        async fn open(&mut self) -> Result<()> {
            Ok(())
        }

        async fn next(&mut self) -> Result<Option<DataChunk>> {
            let chunk = DataChunk::builder()
                .columns(vec![Column::new(
                    Arc::new(array_nonnull! { I32Array, [1, 2, 3] }.into()),
                    Int32Type::create(false),
                )])
                .build();
            Ok(Some(chunk))
        }

        async fn close(&mut self) -> Result<()> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_sctrean_scan() {
        let reader = MockSourceReader {};
        let mut executor = StreamScanExecutor {
            reader: Box::new(reader),
            done: false,
            schema: Schema::new(vec![Field::new(Int32Type::create(false))]),
        };
        executor.open().await.unwrap();

        let chunk1 = executor.next().await.unwrap();
        assert!(chunk1.is_some());
        let chunk1 = chunk1.unwrap();
        assert_eq!(1, chunk1.dimension());
        assert_eq!(
            chunk1
                .column_at(0)
                .unwrap()
                .array()
                .as_int32()
                .iter()
                .collect_vec(),
            vec![Some(1), Some(2), Some(3)]
        );

        let chunk2 = executor.next().await.unwrap();
        assert!(chunk2.is_some());
        let chunk2 = chunk2.unwrap();
        assert_eq!(1, chunk2.dimension());
        assert_eq!(
            chunk2
                .column_at(0)
                .unwrap()
                .array()
                .as_int32()
                .iter()
                .collect_vec(),
            vec![Some(1), Some(2), Some(3)]
        );

        executor.close().await.unwrap();
    }
}