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
pub mod config;
pub mod errors;
pub mod otel;
pub mod types;

pub use errors::SolbotError;
