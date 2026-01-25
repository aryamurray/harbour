//! FFI binding generation for C libraries.
//!
//! This module provides parsing and code generation for creating
//! foreign language bindings from C header files.

pub mod parser;
pub mod types;
pub mod typescript;

pub use parser::HeaderParser;
pub use types::{
    CConstant, CEnum, CEnumVariant, CField, CFunction, CParam, CStruct, CType, CTypedef,
    CallingConvention, ParsedHeader,
};
pub use typescript::TypeScriptGenerator;
