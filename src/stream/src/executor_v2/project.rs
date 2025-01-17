// Copyright 2022 Singularity Data
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::fmt::{Debug, Formatter};

use itertools::Itertools;
use risingwave_common::array::column::Column;
use risingwave_common::array::{DataChunk, StreamChunk};
use risingwave_common::catalog::{Field, Schema};
use risingwave_expr::expr::BoxedExpression;

use super::{Executor, ExecutorInfo, SimpleExecutor, SimpleExecutorWrapper, StreamExecutorResult};
use crate::executor::PkIndicesRef;
use crate::executor_v2::error::StreamExecutorError;

pub type ProjectExecutor = SimpleExecutorWrapper<SimpleProjectExecutor>;

impl ProjectExecutor {
    pub fn new(input: Box<dyn Executor>, exprs: Vec<BoxedExpression>, execuotr_id: u64) -> Self {
        let info = input.info();

        SimpleExecutorWrapper {
            input,
            inner: SimpleProjectExecutor::new(info, exprs, execuotr_id),
        }
    }
}

/// `ProjectExecutor` project data with the `expr`. The `expr` takes a chunk of data,
/// and returns a new data chunk. And then, `ProjectExecutor` will insert, delete
/// or update element into next operator according to the result of the expression.
pub struct SimpleProjectExecutor {
    info: ExecutorInfo,

    /// Expressions of the current projection.
    exprs: Vec<BoxedExpression>,
}

impl SimpleProjectExecutor {
    pub fn new(input_info: ExecutorInfo, exprs: Vec<BoxedExpression>, executor_id: u64) -> Self {
        let schema = Schema {
            fields: exprs
                .iter()
                .map(|e| Field::unnamed(e.return_type()))
                .collect_vec(),
        };
        Self {
            info: ExecutorInfo {
                schema,
                pk_indices: input_info.pk_indices,
                identity: format!("ProjectExecutor {:X}", executor_id),
            },
            exprs,
        }
    }
}

impl Debug for SimpleProjectExecutor {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProjectExecutor")
            .field("exprs", &self.exprs)
            .finish()
    }
}

impl SimpleExecutor for SimpleProjectExecutor {
    fn map_filter_chunk(
        &mut self,
        chunk: StreamChunk,
    ) -> StreamExecutorResult<Option<StreamChunk>> {
        let chunk = chunk.compact().map_err(StreamExecutorError::eval_error)?;

        let (ops, columns, visibility) = chunk.into_inner();
        let data_chunk = {
            let data_chunk_builder = DataChunk::builder().columns(columns);
            if let Some(visibility) = visibility {
                data_chunk_builder.visibility(visibility).build()
            } else {
                data_chunk_builder.build()
            }
        };

        let projected_columns = self
            .exprs
            .iter_mut()
            .map(|expr| {
                expr.eval(&data_chunk)
                    .map(Column::new)
                    .map_err(StreamExecutorError::eval_error)
            })
            .collect::<Result<Vec<Column>, _>>()?;

        let new_chunk = StreamChunk::new(ops, projected_columns, None);
        Ok(Some(new_chunk))
    }

    fn schema(&self) -> &Schema {
        &self.info.schema
    }

    fn pk_indices(&self) -> PkIndicesRef {
        &self.info.pk_indices
    }

    fn identity(&self) -> &str {
        &self.info.identity
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use itertools::Itertools;
    use risingwave_common::array::{I64Array, *};
    use risingwave_common::catalog::{Field, Schema};
    use risingwave_common::column_nonnull;
    use risingwave_common::types::DataType;
    use risingwave_expr::expr::expr_binary_nonnull::new_binary_expr;
    use risingwave_expr::expr::InputRefExpression;
    use risingwave_pb::expr::expr_node::Type;

    use super::super::test_utils::MockSource;
    use super::super::*;
    use super::*;

    #[tokio::test]
    async fn test_projection() {
        let chunk1 = StreamChunk::new(
            vec![Op::Insert, Op::Insert, Op::Insert],
            vec![
                column_nonnull! { I64Array, [1, 2, 3] },
                column_nonnull! { I64Array, [4, 5, 6] },
            ],
            None,
        );
        let chunk2 = StreamChunk::new(
            vec![Op::Insert, Op::Delete],
            vec![
                column_nonnull! { I64Array, [7, 3] },
                column_nonnull! { I64Array, [8, 6] },
            ],
            Some((vec![true, true]).try_into().unwrap()),
        );
        let schema = Schema {
            fields: vec![
                Field::unnamed(DataType::Int64),
                Field::unnamed(DataType::Int64),
            ],
        };
        let source = MockSource::with_chunks(schema, PkIndices::new(), vec![chunk1, chunk2]);

        let left_expr = InputRefExpression::new(DataType::Int64, 0);
        let right_expr = InputRefExpression::new(DataType::Int64, 1);
        let test_expr = new_binary_expr(
            Type::Add,
            DataType::Int64,
            Box::new(left_expr),
            Box::new(right_expr),
        );

        let project = Box::new(ProjectExecutor::new(Box::new(source), vec![test_expr], 1));
        let mut project = project.execute();

        if let Message::Chunk(chunk) = project.next().await.unwrap().unwrap() {
            assert_eq!(chunk.ops(), vec![Op::Insert, Op::Insert, Op::Insert]);
            assert_eq!(chunk.columns().len(), 1);
            assert_eq!(
                chunk
                    .column_at(0)
                    .array_ref()
                    .as_int64()
                    .iter()
                    .collect_vec(),
                vec![Some(5), Some(7), Some(9)]
            );
        } else {
            unreachable!();
        }

        if let Message::Chunk(chunk) = project.next().await.unwrap().unwrap() {
            assert_eq!(chunk.ops(), vec![Op::Insert, Op::Delete]);
            assert_eq!(chunk.columns().len(), 1);
            assert_eq!(
                chunk
                    .column_at(0)
                    .array_ref()
                    .as_int64()
                    .iter()
                    .collect_vec(),
                vec![Some(15), Some(9)]
            );
        } else {
            unreachable!();
        }

        assert!(project.next().await.unwrap().unwrap().is_stop());
    }
}
