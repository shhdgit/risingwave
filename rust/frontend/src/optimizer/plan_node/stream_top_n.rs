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

use std::fmt;

use risingwave_pb::stream_plan::stream_node::Node as ProstStreamNode;

use super::{LogicalTopN, PlanBase, PlanRef, PlanTreeNodeUnary, ToStreamProst};
use crate::optimizer::property::Distribution;

#[derive(Debug, Clone)]
pub struct StreamTopN {
    pub base: PlanBase,
    logical: LogicalTopN,
}

impl StreamTopN {
    pub fn new(logical: LogicalTopN) -> Self {
        let ctx = logical.base.ctx.clone();
        let pk_indices = logical.base.pk_indices.to_vec();
        let input = logical.input();
        let input_dist = input.distribution();
        let dist = match input_dist {
            Distribution::Any => Distribution::Any,
            Distribution::Single => Distribution::Single,
            _ => panic!(),
        };

        // Stream TopN executor might change the append-only behavior of the stream.
        let base = PlanBase::new_stream(ctx, logical.schema().clone(), pk_indices, dist, false);
        StreamTopN { base, logical }
    }
}

impl fmt::Display for StreamTopN {
    fn fmt(&self, _f: &mut fmt::Formatter) -> fmt::Result {
        todo!()
    }
}

impl PlanTreeNodeUnary for StreamTopN {
    fn input(&self) -> PlanRef {
        self.logical.input()
    }

    fn clone_with_input(&self, input: PlanRef) -> Self {
        Self::new(self.logical.clone_with_input(input))
    }
}
impl_plan_tree_node_for_unary! { StreamTopN }

impl ToStreamProst for StreamTopN {
    fn to_stream_prost_body(&self) -> ProstStreamNode {
        use risingwave_pb::stream_plan::*;

        let (distribution_keys, order_types) = self.logical.order.to_protobuf_id_and_order();

        ProstStreamNode::TopNNode(TopNNode {
            limit: self.logical.limit as u64,
            offset: self.logical.offset as u64,
            distribution_keys,
            order_types: order_types.iter().map(|x| *x as i32).collect(),
        })
    }
}