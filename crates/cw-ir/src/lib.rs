//! `cw-ir`: Dimensional type checker, family/reduction expander, scope & epoch DAG resolver.

pub mod expr;
pub mod schema;
pub mod typecheck;

pub use expr::{BinOp, Expr, ReductionOp, parse_expr};
pub use schema::{
    AssertionDecl, BlockLensDecl, ClaimsConfig, ColumnScopeDecl, ConstantDecl, DerivationDecl,
    FamilyDecl, MetricDecl, ScalarClaimDecl, TableLensDecl, TableMode, TableRowSpec,
    parse_deadband,
};
pub use typecheck::{
    CheckedIr, check_binary_op_types, check_claims_config, infer_expr_type, resolve_claim_ref_type,
    resolve_family_field_type, types_compatible,
};
