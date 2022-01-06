use itertools::Itertools;

use super::PlanPass;
use crate::planner::property::{Distribution, Order};
use crate::planner::rule::BoxedRule;
use crate::planner::PlanRef;
#[allow(dead_code)]
/// Traverse order of [`HeuristicOptimizer`]
pub enum ApplyOrder {
    TopDown,
    BottomUp,
}
impl Default for ApplyOrder {
    fn default() -> Self {
        ApplyOrder::TopDown
    }
}
// TODO: we should have a builder of HeuristicOptimizer here
/// a rule based heuristic optimzer, which traverse every nodes and try to apply each rules on them.
pub struct HeuristicOptimizer {
    apply_order: ApplyOrder,
    rules: Vec<BoxedRule>,
}
impl HeuristicOptimizer {
    fn optimize_self_node(&mut self, mut plan: PlanRef) -> PlanRef {
        for rule in &self.rules {
            if let Some(applied) = rule.apply(plan.clone()) {
                plan = applied;
            }
        }
        plan
    }

    fn optimize_children(&mut self, plan: PlanRef) -> PlanRef {
        let order_required = plan.children_order_required();
        let dists_required = plan.children_distribution_required();

        let children = plan
            .children()
            .into_iter()
            .zip_eq(order_required.into_iter())
            .zip_eq(dists_required.into_iter())
            .map(|((sub_tree, order), dist)| self.pass_with_require(sub_tree, order, dist))
            .collect_vec();
        plan.clone_with_children(&children)
    }
}

impl PlanPass for HeuristicOptimizer {
    fn pass_with_require(
        &mut self,
        mut plan: PlanRef,
        required_order: Order,
        required_dist: Distribution,
    ) -> PlanRef {
        plan = match self.apply_order {
            ApplyOrder::TopDown => {
                plan = self.optimize_self_node(plan);
                self.optimize_children(plan)
            }
            ApplyOrder::BottomUp => {
                plan = self.optimize_children(plan);
                self.optimize_self_node(plan)
            }
        };
        plan = required_order.enforce_if_not_satisfies(plan);
        required_dist.enforce_if_not_satisfies(plan, &required_order)
    }
}