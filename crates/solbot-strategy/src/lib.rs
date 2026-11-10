#![allow(
    deprecated,
    clippy::result_large_err,
    clippy::double_must_use,
    clippy::too_many_arguments,
    clippy::type_complexity,
    clippy::needless_range_loop,
    clippy::if_same_then_else,
    clippy::collapsible_if,
    clippy::unnecessary_map_or,
    clippy::clone_on_copy,
    clippy::match_like_matches_macro,
    clippy::single_match,
    clippy::question_mark,
    clippy::needless_borrows_for_generic_args,
    clippy::unnecessary_min_or_max,
    clippy::unnecessary_cast,
    clippy::get_first,
    clippy::useless_format,
    clippy::needless_borrow
)]
pub mod actor;
pub mod alpha_layer;
pub mod circuit_breaker;
pub mod exit;
pub mod observability;
pub mod paper_trade_log;
pub mod pipeline_bench;
pub mod pnl;
pub mod risk_gate;
pub mod routing;
pub mod signal;
pub mod signer_client;
pub mod sizing;
pub mod smart_money;
pub mod startup;
pub mod state;
pub mod tx_builder;
pub mod wsol;
