//! Type definitions for parsed C headers.
//!
//! These types represent the FFI-relevant information extracted from C headers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A parsed C header file containing all FFI-relevant information.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ParsedHeader {
    /// Source file path
    pub source: PathBuf,

    /// Parsed functions
    pub functions: Vec<CFunction>,

    /// Parsed structures
    pub structs: Vec<CStruct>,

    /// Parsed enumerations
    pub enums: Vec<CEnum>,

    /// Parsed typedefs
    pub typedefs: Vec<CTypedef>,

    /// Parsed macros/constants
    pub constants: Vec<CConstant>,
}

impl ParsedHeader {
    /// Create a new empty parsed header.
    pub fn new(source: impl Into<PathBuf>) -> Self {
        ParsedHeader {
            source: source.into(),
            ..Default::default()
        }
    }

    /// Merge another parsed header into this one.
    pub fn merge(&mut self, other: ParsedHeader) {
        self.functions.extend(other.functions);
        self.structs.extend(other.structs);
        self.enums.extend(other.enums);
        self.typedefs.extend(other.typedefs);
        self.constants.extend(other.constants);
    }
}

/// A C function declaration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CFunction {
    /// Function name
    pub name: String,

    /// Return type
    pub return_type: CType,

    /// Function parameters
    pub params: Vec<CParam>,

    /// Calling convention (cdecl, stdcall, etc.)
    pub calling_convention: CallingConvention,

    /// Whether this is a variadic function
    pub variadic: bool,

    /// Documentation comment (if any)
    pub doc: Option<String>,
}

impl CFunction {
    /// Create a new function with the given name and return type.
    pub fn new(name: impl Into<String>, return_type: CType) -> Self {
        CFunction {
            name: name.into(),
            return_type,
            params: Vec::new(),
            calling_convention: CallingConvention::Cdecl,
            variadic: false,
            doc: None,
        }
    }

    /// Add a parameter.
    pub fn with_param(mut self, param: CParam) -> Self {
        self.params.push(param);
        self
    }
}

/// A function parameter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CParam {
    /// Parameter name (may be empty for unnamed params)
    pub name: String,

    /// Parameter type
    pub param_type: CType,
}

impl CParam {
    /// Create a new parameter.
    pub fn new(name: impl Into<String>, param_type: CType) -> Self {
        CParam {
            name: name.into(),
            param_type,
        }
    }
}

/// Calling conventions for FFI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CallingConvention {
    /// C calling convention (default)
    #[default]
    Cdecl,
    /// Windows stdcall
    Stdcall,
    /// Windows fastcall
    Fastcall,
    /// System V AMD64 ABI
    SysV,
    /// Windows x64 calling convention
    Win64,
}

impl CallingConvention {
    /// Get the koffi calling convention string.
    pub fn as_koffi(&self) -> &'static str {
        match self {
            CallingConvention::Cdecl => "cdecl",
            CallingConvention::Stdcall => "stdcall",
            CallingConvention::Fastcall => "fastcall",
            CallingConvention::SysV => "sysv",
            CallingConvention::Win64 => "win64",
        }
    }
}

/// A C structure definition.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CStruct {
    /// Struct name
    pub name: String,

    /// Struct fields
    pub fields: Vec<CField>,

    /// Whether this is a packed struct
    pub packed: bool,

    /// Documentation comment
    pub doc: Option<String>,
}

impl CStruct {
    /// Create a new struct.
    pub fn new(name: impl Into<String>) -> Self {
        CStruct {
            name: name.into(),
            fields: Vec::new(),
            packed: false,
            doc: None,
        }
    }

    /// Add a field.
    pub fn with_field(mut self, field: CField) -> Self {
        self.fields.push(field);
        self
    }
}

/// A struct field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CField {
    /// Field name
    pub name: String,

    /// Field type
    pub field_type: CType,

    /// Bit width for bitfields (None for regular fields)
    pub bit_width: Option<u32>,
}

impl CField {
    /// Create a new field.
    pub fn new(name: impl Into<String>, field_type: CType) -> Self {
        CField {
            name: name.into(),
            field_type,
            bit_width: None,
        }
    }
}

/// A C enumeration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CEnum {
    /// Enum name
    pub name: String,

    /// Enum variants
    pub variants: Vec<CEnumVariant>,

    /// Documentation comment
    pub doc: Option<String>,
}

impl CEnum {
    /// Create a new enum.
    pub fn new(name: impl Into<String>) -> Self {
        CEnum {
            name: name.into(),
            variants: Vec::new(),
            doc: None,
        }
    }

    /// Add a variant.
    pub fn with_variant(mut self, variant: CEnumVariant) -> Self {
        self.variants.push(variant);
        self
    }
}

/// An enum variant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CEnumVariant {
    /// Variant name
    pub name: String,

    /// Explicit value (if any)
    pub value: Option<i64>,
}

impl CEnumVariant {
    /// Create a new variant.
    pub fn new(name: impl Into<String>) -> Self {
        CEnumVariant {
            name: name.into(),
            value: None,
        }
    }

    /// Set the value.
    pub fn with_value(mut self, value: i64) -> Self {
        self.value = Some(value);
        self
    }
}

/// A C typedef.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CTypedef {
    /// New type name
    pub name: String,

    /// Underlying type
    pub underlying_type: CType,

    /// Documentation comment
    pub doc: Option<String>,
}

impl CTypedef {
    /// Create a new typedef.
    pub fn new(name: impl Into<String>, underlying_type: CType) -> Self {
        CTypedef {
            name: name.into(),
            underlying_type,
            doc: None,
        }
    }
}

/// A C constant (from #define or const).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CConstant {
    /// Constant name
    pub name: String,

    /// Constant value as string
    pub value: String,

    /// Inferred type (if possible)
    pub const_type: Option<CType>,
}

impl CConstant {
    /// Create a new constant.
    pub fn new(name: impl Into<String>, value: impl Into<String>) -> Self {
        CConstant {
            name: name.into(),
            value: value.into(),
            const_type: None,
        }
    }
}

/// C type representation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CType {
    /// Void type
    Void,

    /// Signed integer types
    Int8,
    Int16,
    Int32,
    Int64,

    /// Unsigned integer types
    UInt8,
    UInt16,
    UInt32,
    UInt64,

    /// Floating point types
    Float,
    Double,

    /// Boolean
    Bool,

    /// Character types
    Char,
    UChar,
    WChar,

    /// Size types
    Size,
    SSize,
    PtrDiff,

    /// Pointer to another type
    Pointer(Box<CType>),

    /// Const pointer
    ConstPointer(Box<CType>),

    /// Fixed-size array
    Array(Box<CType>, usize),

    /// Reference to a struct
    Struct(String),

    /// Reference to an enum
    Enum(String),

    /// Reference to a typedef
    TypeDef(String),

    /// Function pointer
    FunctionPointer {
        return_type: Box<CType>,
        param_types: Vec<CType>,
    },

    /// Unknown/opaque type
    Opaque(String),
}

impl CType {
    /// Parse a C type string.
    pub fn parse(s: &str) -> Self {
        let s = s.trim();

        // Handle pointers
        if let Some(inner) = s.strip_suffix('*') {
            let inner = inner.trim();
            if let Some(stripped) = inner.strip_prefix("const ") {
                return CType::ConstPointer(Box::new(CType::parse(stripped)));
            }
            return CType::Pointer(Box::new(CType::parse(inner)));
        }

        // Handle const prefix
        let s = s.strip_prefix("const ").unwrap_or(s);

        // Handle unsigned prefix
        let (is_unsigned, s) = if let Some(stripped) = s.strip_prefix("unsigned ") {
            (true, stripped)
        } else {
            (false, s)
        };

        // Handle signed prefix (usually explicit)
        let s = s.strip_prefix("signed ").unwrap_or(s);

        match s {
            "void" => CType::Void,
            "bool" | "_Bool" => CType::Bool,
            "char" if is_unsigned => CType::UChar,
            "char" => CType::Char,
            "wchar_t" => CType::WChar,
            "short" | "short int" if is_unsigned => CType::UInt16,
            "short" | "short int" => CType::Int16,
            "int" if is_unsigned => CType::UInt32,
            "int" => CType::Int32,
            "long" | "long int" if is_unsigned => {
                // long is platform-dependent, assume 64-bit on modern systems
                CType::UInt64
            }
            "long" | "long int" => CType::Int64,
            "long long" | "long long int" if is_unsigned => CType::UInt64,
            "long long" | "long long int" => CType::Int64,
            "float" => CType::Float,
            "double" => CType::Double,
            "long double" => CType::Double, // Simplified - long double is complex

            // Fixed-width types
            "int8_t" | "__int8" => CType::Int8,
            "int16_t" | "__int16" => CType::Int16,
            "int32_t" | "__int32" => CType::Int32,
            "int64_t" | "__int64" => CType::Int64,
            "uint8_t" => CType::UInt8,
            "uint16_t" => CType::UInt16,
            "uint32_t" => CType::UInt32,
            "uint64_t" => CType::UInt64,

            // Size types
            "size_t" => CType::Size,
            "ssize_t" => CType::SSize,
            "ptrdiff_t" => CType::PtrDiff,
            "intptr_t" => CType::Int64,
            "uintptr_t" => CType::UInt64,

            // Named types
            other => {
                if let Some(stripped) = other.strip_prefix("struct ") {
                    CType::Struct(stripped.to_string())
                } else if let Some(stripped) = other.strip_prefix("enum ") {
                    CType::Enum(stripped.to_string())
                } else {
                    CType::TypeDef(other.to_string())
                }
            }
        }
    }

    /// Check if this is a pointer type.
    pub fn is_pointer(&self) -> bool {
        matches!(self, CType::Pointer(_) | CType::ConstPointer(_))
    }

    /// Check if this is a void type.
    pub fn is_void(&self) -> bool {
        matches!(self, CType::Void)
    }

    /// Get the koffi type name.
    pub fn as_koffi(&self) -> String {
        match self {
            CType::Void => "void".to_string(),
            CType::Int8 => "int8".to_string(),
            CType::Int16 => "int16".to_string(),
            CType::Int32 => "int32".to_string(),
            CType::Int64 => "int64".to_string(),
            CType::UInt8 => "uint8".to_string(),
            CType::UInt16 => "uint16".to_string(),
            CType::UInt32 => "uint32".to_string(),
            CType::UInt64 => "uint64".to_string(),
            CType::Float => "float32".to_string(),
            CType::Double => "float64".to_string(),
            CType::Bool => "bool".to_string(),
            CType::Char => "char".to_string(),
            CType::UChar => "uchar".to_string(),
            CType::WChar => "int16".to_string(), // Platform-dependent
            CType::Size => "size_t".to_string(),
            CType::SSize => "ssize_t".to_string(),
            CType::PtrDiff => "intptr".to_string(),
            CType::Pointer(inner) if inner.is_void() => "pointer".to_string(),
            CType::Pointer(inner) => format!("{}*", inner.as_koffi()),
            CType::ConstPointer(inner) if inner.is_void() => "pointer".to_string(),
            CType::ConstPointer(inner) => format!("{}*", inner.as_koffi()),
            CType::Array(inner, size) => format!("koffi.array({}, {})", inner.as_koffi(), size),
            CType::Struct(name) => name.clone(),
            CType::Enum(name) => name.clone(),
            CType::TypeDef(name) => name.clone(),
            CType::FunctionPointer { .. } => "pointer".to_string(),
            CType::Opaque(_) => "pointer".to_string(),
        }
    }

    /// Get the TypeScript type.
    pub fn as_typescript(&self) -> String {
        match self {
            CType::Void => "void".to_string(),
            CType::Int8
            | CType::Int16
            | CType::Int32
            | CType::UInt8
            | CType::UInt16
            | CType::UInt32
            | CType::Float
            | CType::Double => "number".to_string(),
            CType::Int64 | CType::UInt64 | CType::Size | CType::SSize | CType::PtrDiff => {
                "bigint".to_string()
            }
            CType::Bool => "boolean".to_string(),
            CType::Char | CType::UChar | CType::WChar => "number".to_string(),
            CType::Pointer(inner) if matches!(**inner, CType::Char) => {
                "string | Buffer".to_string()
            }
            CType::ConstPointer(inner) if matches!(**inner, CType::Char) => "string".to_string(),
            CType::Pointer(_) | CType::ConstPointer(_) => "Buffer | number".to_string(),
            CType::Array(inner, _) => format!("{}[]", inner.as_typescript()),
            CType::Struct(name) => name.clone(),
            CType::Enum(name) => name.clone(),
            CType::TypeDef(name) => name.clone(),
            CType::FunctionPointer { .. } => "Function".to_string(),
            CType::Opaque(_) => "number".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ctype_parse() {
        assert_eq!(CType::parse("void"), CType::Void);
        assert_eq!(CType::parse("int"), CType::Int32);
        assert_eq!(CType::parse("unsigned int"), CType::UInt32);
        assert_eq!(CType::parse("int64_t"), CType::Int64);
        assert_eq!(CType::parse("char*"), CType::Pointer(Box::new(CType::Char)));
        assert_eq!(
            CType::parse("const char*"),
            CType::ConstPointer(Box::new(CType::Char))
        );
        assert_eq!(
            CType::parse("struct MyStruct"),
            CType::Struct("MyStruct".to_string())
        );
    }

    #[test]
    fn test_ctype_as_koffi() {
        assert_eq!(CType::Int32.as_koffi(), "int32");
        assert_eq!(CType::Pointer(Box::new(CType::Void)).as_koffi(), "pointer");
        assert_eq!(CType::Pointer(Box::new(CType::Int32)).as_koffi(), "int32*");
    }

    #[test]
    fn test_ctype_as_typescript() {
        assert_eq!(CType::Int32.as_typescript(), "number");
        assert_eq!(CType::Int64.as_typescript(), "bigint");
        assert_eq!(
            CType::Pointer(Box::new(CType::Char)).as_typescript(),
            "string | Buffer"
        );
    }
}
