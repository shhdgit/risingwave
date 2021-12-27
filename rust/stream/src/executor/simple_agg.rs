//! Streaming Aggregators

use std::sync::Arc;

use async_trait::async_trait;
use itertools::Itertools;
use risingwave_common::array::column::Column;
use risingwave_common::array::*;
use risingwave_common::catalog::Schema;
use risingwave_common::error::Result;
use risingwave_storage::{Keyspace, StateStore};

use super::aggregation::*;
use super::{pk_input_arrays, Barrier, Executor, Message, PkIndices, PkIndicesRef};

/// `SimpleAggExecutor` is the aggregation operator for streaming system.
/// To create an aggregation operator, states and expressions should be passed along the
/// constructor.
///
/// `SimpleAggExecutor` maintain multiple states together. If there are `n`
/// states and `n` expressions, there will be `n` columns as output.
///
/// As the engine processes data in chunks, it is possible that multiple update
/// messages could consolidate to a single row update. For example, our source
/// emits 1000 inserts in one chunk, and we aggregates count function on that.
/// Current `SimpleAggExecutor` will only emit one row for a whole chunk.
/// Therefore, we "automatically" implemented a window function inside
/// `SimpleAggExecutor`.
pub struct SimpleAggExecutor<S: StateStore> {
    /// Schema of the executor.
    schema: Schema,

    /// Primary key indices.
    pk_indices: PkIndices,

    /// If `next_barrier_message` exists, we should send a Barrier while next called.
    // TODO: This can be optimized while async gen fn stablized.
    cached_barrier_message: Option<Barrier>,

    /// The executor operates on this keyspace.
    keyspace: Keyspace<S>,

    /// Aggregation states of the current operator.
    /// This is an `Option` and the initial state is built when `Executor::next` is called, since
    /// we may not want `Self::new` to be an `async` function.
    states: Option<AggState<S>>,

    /// The input of the current operator.
    input: Box<dyn Executor>,

    /// An operator will support multiple aggregation calls.
    agg_calls: Vec<AggCall>,
}

impl<S: StateStore> SimpleAggExecutor<S> {
    pub fn new(
        input: Box<dyn Executor>,
        agg_calls: Vec<AggCall>,
        keyspace: Keyspace<S>,
        pk_indices: PkIndices,
    ) -> Self {
        // simple agg does not have group key
        let schema = generate_agg_schema(input.as_ref(), &agg_calls, None);

        Self {
            schema,
            pk_indices,
            cached_barrier_message: None,
            keyspace,
            states: None,
            input,
            agg_calls,
        }
    }
}

#[async_trait]
impl<S: StateStore> AggExecutor for SimpleAggExecutor<S> {
    fn cached_barrier_message_mut(&mut self) -> &mut Option<Barrier> {
        &mut self.cached_barrier_message
    }

    async fn apply_chunk(&mut self, chunk: StreamChunk) -> Result<()> {
        let StreamChunk {
            ops,
            columns,
            visibility,
        } = chunk;

        // --- Retrieve all aggregation inputs in advance ---
        let all_agg_input_arrays = agg_input_arrays(&self.agg_calls, &columns);
        let pk_input_arrays = pk_input_arrays(self.input.pk_indices(), &columns);
        let input_pk_data_type_kinds = self.input.pk_data_type_kinds();

        // When applying batch, we will send columns of primary keys to the last N columns.
        let all_agg_data = all_agg_input_arrays
            .into_iter()
            .map(|mut input_arrays| {
                input_arrays.extend(pk_input_arrays.iter());
                input_arrays
            })
            .collect_vec();

        // 1. Retrieve previous state from the KeyedState. If they didn't exist, the ManagedState
        // will automatically create new ones for them.
        if self.states.is_none() {
            let state = generate_agg_state(
                None,
                &self.agg_calls,
                &self.keyspace,
                input_pk_data_type_kinds,
            )
            .await?;
            self.states = Some(state);
        }
        let states = self.states.as_mut().unwrap();

        // 2. Mark the state as dirty by filling prev states
        states.may_mark_as_dirty().await?;

        // 3. Apply batch to each of the state (per agg_call)
        for (agg_state, data) in states.managed_states.iter_mut().zip(all_agg_data.iter()) {
            agg_state
                .apply_batch(&ops, visibility.as_ref(), data)
                .await?;
        }

        Ok(())
    }

    async fn flush_data(&mut self) -> Result<Option<StreamChunk>> {
        // --- Flush states to the state store ---
        // Some state will have the correct output only after their internal states have been fully
        // flushed.

        let states = match self.states.as_mut() {
            Some(states) if states.is_dirty() => states,
            _ => return Ok(None), // Nothing to flush.
        };

        let mut write_batch = vec![];
        for state in &mut states.managed_states {
            state.flush(&mut write_batch)?;
        }

        self.keyspace
            .state_store()
            .ingest_batch(write_batch)
            .await?;

        // --- Create array builders ---
        // As the datatype is retrieved from schema, it contains both group key and aggregation
        // state outputs.
        let mut builders = self.schema.create_array_builders(2)?;
        let mut new_ops = Vec::with_capacity(2);

        // --- Retrieve modified states and put the changes into the builders ---
        let _is_empty = states
            .build_changes(&mut builders, &mut new_ops, None)
            .await?;

        let columns: Vec<Column> = builders
            .into_iter()
            .map(|builder| -> Result<_> { Ok(Column::new(Arc::new(builder.finish()?))) })
            .try_collect()?;

        let chunk = StreamChunk {
            ops: new_ops,
            columns,
            visibility: None,
        };

        Ok(Some(chunk))
    }

    fn input(&mut self) -> &mut dyn Executor {
        self.input.as_mut()
    }
}

#[async_trait]
impl<S: StateStore> Executor for SimpleAggExecutor<S> {
    async fn next(&mut self) -> Result<Message> {
        agg_executor_next(self).await
    }

    fn schema(&self) -> &Schema {
        &self.schema
    }

    fn pk_indices(&self) -> PkIndicesRef {
        &self.pk_indices
    }
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use risingwave_common::catalog::Field;
    use risingwave_common::column_nonnull;
    use risingwave_common::expr::*;
    use risingwave_common::types::*;

    use super::*;
    use crate::executor::test_utils::*;
    use crate::executor::*;
    use crate::*;

    #[tokio::test]
    async fn test_local_simple_aggregation_in_memory() {
        test_local_simple_aggregation(create_in_memory_keyspace()).await
    }

    async fn test_local_simple_aggregation(keyspace: Keyspace<impl StateStore>) {
        let chunk1 = StreamChunk {
            ops: vec![Op::Insert, Op::Insert, Op::Insert],
            columns: vec![
                column_nonnull! { I64Array, Int64Type, [100, 10, 4] },
                column_nonnull! { I64Array, Int64Type, [200, 14, 300] },
                // primary key column
                column_nonnull! { I64Array, Int64Type, [1001, 1002, 1003] },
            ],
            visibility: None,
        };
        let chunk2 = StreamChunk {
            ops: vec![Op::Delete, Op::Delete, Op::Delete, Op::Insert],
            columns: vec![
                column_nonnull! { I64Array, Int64Type, [100, 10, 4, 104] },
                column_nonnull! { I64Array, Int64Type, [200, 14, 300, 500] },
                // primary key column
                column_nonnull! { I64Array, Int64Type, [1001, 1002, 1003, 1004] },
            ],
            visibility: Some((vec![true, false, true, true]).try_into().unwrap()),
        };
        let schema = Schema {
            fields: vec![
                Field {
                    data_type: Int64Type::create(false),
                },
                Field {
                    data_type: Int64Type::create(false),
                },
                // primary key column
                Field {
                    data_type: Int64Type::create(false),
                },
            ],
        };

        let mut source = MockSource::new(schema, vec![2]); // pk
        source.push_chunks([chunk1].into_iter());
        source.push_barrier(1, false);
        source.push_chunks([chunk2].into_iter());
        source.push_barrier(2, false);

        // This is local simple aggregation, so we add another row count state
        let agg_calls = vec![
            AggCall {
                kind: AggKind::RowCount,
                args: AggArgs::None,
                return_type: Int64Type::create(false),
            },
            AggCall {
                kind: AggKind::Sum,
                args: AggArgs::Unary(Int64Type::create(false), 0),
                return_type: Int64Type::create(false),
            },
            AggCall {
                kind: AggKind::Sum,
                args: AggArgs::Unary(Int64Type::create(false), 1),
                return_type: Int64Type::create(false),
            },
            AggCall {
                kind: AggKind::Min,
                args: AggArgs::Unary(Int64Type::create(false), 0),
                return_type: Int64Type::create(false),
            },
        ];

        let mut simple_agg = SimpleAggExecutor::new(Box::new(source), agg_calls, keyspace, vec![]);

        let msg = simple_agg.next().await.unwrap();
        if let Message::Chunk(chunk) = msg {
            let (data_chunk, ops) = chunk.into_parts();
            let rows = ops
                .into_iter()
                .zip(data_chunk.rows().map(Row::from))
                .collect_vec();
            let expected_rows = [(Op::Insert, row_nonnull![3_i64, 114_i64, 514_i64, 4_i64])];

            assert_eq!(rows, expected_rows);
        } else {
            unreachable!("unexpected message {:?}", msg);
        }

        assert_matches!(simple_agg.next().await.unwrap(), Message::Barrier { .. });

        let msg = simple_agg.next().await.unwrap();
        if let Message::Chunk(chunk) = msg {
            let (data_chunk, ops) = chunk.into_parts();
            let rows = ops
                .into_iter()
                .zip(data_chunk.rows().map(Row::from))
                .collect_vec();
            let expected_rows = [
                (
                    Op::UpdateDelete,
                    row_nonnull![3_i64, 114_i64, 514_i64, 4_i64],
                ),
                (
                    Op::UpdateInsert,
                    row_nonnull![2_i64, 114_i64, 514_i64, 10_i64],
                ),
            ];

            assert_eq!(rows, expected_rows);
        } else {
            unreachable!("unexpected message {:?}", msg);
        }
    }
}