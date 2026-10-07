//! Shared contracts for Jansori database backends.
//!
//! MySQL and PostgreSQL adapters implement [`backend::DatabaseBackend`].
//! Collection, HTTP, and UI code consume only this contract.

pub mod backend;
pub mod model;
pub mod mysql;
pub mod postgres;
pub mod timeline;
