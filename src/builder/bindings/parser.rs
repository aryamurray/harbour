//! C header parser for FFI binding generation.
//!
//! Parses C header files to extract functions, structs, enums, and typedefs
//! for generating foreign language bindings.

use std::path::Path;

use anyhow::{Context, Result};
use regex::Regex;

use super::types::{
    CConstant, CEnum, CEnumVariant, CField, CFunction, CParam, CStruct, CType, CTypedef,
    CallingConvention, ParsedHeader,
};

/// Parser for C header files.
pub struct HeaderParser {
    /// Functions to include (empty = all)
    include_functions: Vec<String>,
    /// Functions to exclude
    exclude_functions: Vec<String>,
    /// Types to include (empty = all)
    include_types: Vec<String>,
    /// Types to exclude
    exclude_types: Vec<String>,
    /// Prefix to strip from names
    strip_prefix: Option<String>,
}

impl Default for HeaderParser {
    fn default() -> Self {
        HeaderParser {
            include_functions: Vec::new(),
            exclude_functions: Vec::new(),
            include_types: Vec::new(),
            exclude_types: Vec::new(),
            strip_prefix: None,
        }
    }
}

impl HeaderParser {
    /// Create a new header parser.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set functions to include.
    pub fn with_include_functions(mut self, funcs: Vec<String>) -> Self {
        self.include_functions = funcs;
        self
    }

    /// Set functions to exclude.
    pub fn with_exclude_functions(mut self, funcs: Vec<String>) -> Self {
        self.exclude_functions = funcs;
        self
    }

    /// Set types to include.
    pub fn with_include_types(mut self, types: Vec<String>) -> Self {
        self.include_types = types;
        self
    }

    /// Set types to exclude.
    pub fn with_exclude_types(mut self, types: Vec<String>) -> Self {
        self.exclude_types = types;
        self
    }

    /// Set prefix to strip from names.
    pub fn with_strip_prefix(mut self, prefix: Option<String>) -> Self {
        self.strip_prefix = prefix;
        self
    }

    /// Parse a header file.
    pub fn parse_file(&self, path: &Path) -> Result<ParsedHeader> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read header: {}", path.display()))?;

        self.parse_content(&content, path)
    }

    /// Parse header content.
    pub fn parse_content(&self, content: &str, path: &Path) -> Result<ParsedHeader> {
        let mut header = ParsedHeader::new(path);

        // Preprocess: remove comments, normalize whitespace
        let preprocessed = self.preprocess(content);

        // Parse functions
        header.functions = self.parse_functions(&preprocessed);

        // Parse structs
        header.structs = self.parse_structs(&preprocessed);

        // Parse enums
        header.enums = self.parse_enums(&preprocessed);

        // Parse typedefs
        header.typedefs = self.parse_typedefs(&preprocessed);

        // Parse constants
        header.constants = self.parse_constants(content); // Use original for #defines

        Ok(header)
    }

    /// Preprocess header content.
    fn preprocess(&self, content: &str) -> String {
        // Remove C-style comments
        let re_block = Regex::new(r"/\*[\s\S]*?\*/").unwrap();
        let content = re_block.replace_all(content, " ");

        // Remove C++ style comments
        let re_line = Regex::new(r"//[^\n]*").unwrap();
        let content = re_line.replace_all(&content, " ");

        // Normalize whitespace
        let re_ws = Regex::new(r"\s+").unwrap();
        re_ws.replace_all(&content, " ").to_string()
    }

    /// Parse function declarations.
    fn parse_functions(&self, content: &str) -> Vec<CFunction> {
        let mut functions = Vec::new();

        // Match function declarations with a simpler pattern:
        // return_type [calling_conv] func_name(params);
        // This regex handles common patterns but won't catch everything
        let re = Regex::new(
            r"(?:extern\s+)?(?:static\s+|inline\s+)*([\w\s*]+?)\s+(__cdecl\s+|__stdcall\s+|__fastcall\s+|WINAPI\s+)?(\w+)\s*\(([^)]*)\)\s*;"
        ).unwrap();

        for cap in re.captures_iter(content) {
            let return_type_str = cap.get(1).map_or("", |m| m.as_str()).trim();
            let calling_conv_str = cap.get(2).map_or("", |m| m.as_str()).trim();
            let name = cap.get(3).map_or("", |m| m.as_str()).trim();
            let params_str = cap.get(4).map_or("", |m| m.as_str()).trim();

            // Skip if filtered out
            if !self.should_include_function(name) {
                continue;
            }

            // Skip common false positives
            if return_type_str.is_empty() || name.is_empty() {
                continue;
            }

            // Parse return type
            let return_type = CType::parse(return_type_str);

            // Parse calling convention
            let calling_convention = match calling_conv_str.to_lowercase().as_str() {
                "__stdcall" | "winapi" => CallingConvention::Stdcall,
                "__fastcall" => CallingConvention::Fastcall,
                _ => CallingConvention::Cdecl,
            };

            // Parse parameters
            let (params, variadic) = self.parse_params(params_str);

            // Strip prefix if configured
            let final_name = self.maybe_strip_prefix(name);

            functions.push(CFunction {
                name: final_name,
                return_type,
                params,
                calling_convention,
                variadic,
                doc: None,
            });
        }

        functions
    }

    /// Parse function parameters.
    fn parse_params(&self, params_str: &str) -> (Vec<CParam>, bool) {
        let mut params = Vec::new();
        let mut variadic = false;

        if params_str.trim() == "void" || params_str.trim().is_empty() {
            return (params, false);
        }

        for param in params_str.split(',') {
            let param = param.trim();

            if param == "..." {
                variadic = true;
                continue;
            }

            // Split into type and name
            // This is a simplified parser - real C parsing is more complex
            if let Some((param_type, param_name)) = self.split_param(param) {
                params.push(CParam {
                    name: param_name,
                    param_type: CType::parse(&param_type),
                });
            } else {
                // Unnamed parameter - just type
                params.push(CParam {
                    name: String::new(),
                    param_type: CType::parse(param),
                });
            }
        }

        (params, variadic)
    }

    /// Split a parameter into type and name.
    fn split_param(&self, param: &str) -> Option<(String, String)> {
        let param = param.trim();

        // Handle pointer parameters: "int *foo" or "int* foo" or "int *"
        if param.contains('*') {
            // Find the last word as the name
            let parts: Vec<&str> = param.split_whitespace().collect();
            if parts.len() >= 2 {
                let last = *parts.last().unwrap();
                if !last.contains('*') {
                    // Last part is the name
                    let name = last.to_string();
                    let type_part = param[..param.len() - last.len()].trim().to_string();
                    return Some((type_part, name));
                }
            }
        }

        // No pointer - split on last whitespace
        let mut parts: Vec<&str> = param.rsplitn(2, char::is_whitespace).collect();
        parts.reverse();

        if parts.len() == 2 {
            let type_str = parts[0].to_string();
            let name = parts[1].to_string();

            // Make sure name looks like an identifier
            if name.chars().all(|c| c.is_alphanumeric() || c == '_') {
                return Some((type_str, name));
            }
        }

        None
    }

    /// Parse struct definitions.
    fn parse_structs(&self, content: &str) -> Vec<CStruct> {
        let mut structs = Vec::new();

        // Match: struct name { fields };
        // or: typedef struct { fields } name;
        let re = Regex::new(
            r"(?:typedef\s+)?struct\s+(\w+)?\s*\{([^}]*)\}\s*(\w+)?\s*;"
        ).unwrap();

        for cap in re.captures_iter(content) {
            let struct_name = cap.get(1).map_or("", |m| m.as_str());
            let body = cap.get(2).map_or("", |m| m.as_str());
            let typedef_name = cap.get(3).map_or("", |m| m.as_str());

            // Prefer typedef name over struct name
            let name = if !typedef_name.is_empty() {
                typedef_name
            } else if !struct_name.is_empty() {
                struct_name
            } else {
                continue; // Anonymous struct
            };

            if !self.should_include_type(name) {
                continue;
            }

            let fields = self.parse_fields(body);
            let final_name = self.maybe_strip_prefix(name);

            structs.push(CStruct {
                name: final_name,
                fields,
                packed: content.contains("__attribute__((packed))"),
                doc: None,
            });
        }

        structs
    }

    /// Parse struct fields.
    fn parse_fields(&self, body: &str) -> Vec<CField> {
        let mut fields = Vec::new();

        for line in body.split(';') {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Handle bitfields: type name : width
            let (line, bit_width) = if line.contains(':') {
                let parts: Vec<&str> = line.splitn(2, ':').collect();
                let width: Option<u32> = parts.get(1).and_then(|w| w.trim().parse().ok());
                (parts[0], width)
            } else {
                (line, None)
            };

            // Split into type and name
            if let Some((field_type, field_name)) = self.split_param(line) {
                fields.push(CField {
                    name: field_name,
                    field_type: CType::parse(&field_type),
                    bit_width,
                });
            }
        }

        fields
    }

    /// Parse enum definitions.
    fn parse_enums(&self, content: &str) -> Vec<CEnum> {
        let mut enums = Vec::new();

        // Match: enum name { variants };
        // or: typedef enum { variants } name;
        let re = Regex::new(
            r"(?:typedef\s+)?enum\s+(\w+)?\s*\{([^}]*)\}\s*(\w+)?\s*;"
        ).unwrap();

        for cap in re.captures_iter(content) {
            let enum_name = cap.get(1).map_or("", |m| m.as_str());
            let body = cap.get(2).map_or("", |m| m.as_str());
            let typedef_name = cap.get(3).map_or("", |m| m.as_str());

            let name = if !typedef_name.is_empty() {
                typedef_name
            } else if !enum_name.is_empty() {
                enum_name
            } else {
                continue;
            };

            if !self.should_include_type(name) {
                continue;
            }

            let variants = self.parse_enum_variants(body);
            let final_name = self.maybe_strip_prefix(name);

            enums.push(CEnum {
                name: final_name,
                variants,
                doc: None,
            });
        }

        enums
    }

    /// Parse enum variants.
    fn parse_enum_variants(&self, body: &str) -> Vec<CEnumVariant> {
        let mut variants = Vec::new();

        for item in body.split(',') {
            let item = item.trim();
            if item.is_empty() {
                continue;
            }

            // Handle: NAME or NAME = VALUE
            if item.contains('=') {
                let parts: Vec<&str> = item.splitn(2, '=').collect();
                let name = parts[0].trim().to_string();
                let value: Option<i64> = parts.get(1).and_then(|v| {
                    let v = v.trim();
                    // Handle hex values
                    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
                        i64::from_str_radix(hex, 16).ok()
                    } else {
                        v.parse().ok()
                    }
                });

                variants.push(CEnumVariant { name, value });
            } else {
                variants.push(CEnumVariant {
                    name: item.to_string(),
                    value: None,
                });
            }
        }

        variants
    }

    /// Parse typedef definitions.
    fn parse_typedefs(&self, content: &str) -> Vec<CTypedef> {
        let mut typedefs = Vec::new();

        // Match simple typedefs: typedef type name;
        // Note: regex crate doesn't support look-ahead, so we filter manually
        let re = Regex::new(
            r"typedef\s+([\w\s*]+)\s+(\w+)\s*;"
        ).unwrap();

        for cap in re.captures_iter(content) {
            let underlying = cap.get(1).map_or("", |m| m.as_str()).trim();
            let name = cap.get(2).map_or("", |m| m.as_str());

            // Skip struct/enum typedefs (handled by parse_structs/parse_enums)
            if underlying.starts_with("struct") || underlying.starts_with("enum") {
                continue;
            }

            if !self.should_include_type(name) {
                continue;
            }

            let final_name = self.maybe_strip_prefix(name);

            typedefs.push(CTypedef {
                name: final_name,
                underlying_type: CType::parse(underlying),
                doc: None,
            });
        }

        typedefs
    }

    /// Parse #define constants.
    fn parse_constants(&self, content: &str) -> Vec<CConstant> {
        let mut constants = Vec::new();

        // Match: #define NAME VALUE
        // Skip function-like macros
        let re = Regex::new(r"#define\s+(\w+)\s+([^\n\\]+)").unwrap();

        for cap in re.captures_iter(content) {
            let name = cap.get(1).map_or("", |m| m.as_str());
            let value = cap.get(2).map_or("", |m| m.as_str()).trim();

            // Skip if looks like a function macro (has parentheses after name in original)
            if content.contains(&format!("#define {}(", name)) {
                continue;
            }

            // Try to infer type from value
            let const_type = self.infer_constant_type(value);

            constants.push(CConstant {
                name: name.to_string(),
                value: value.to_string(),
                const_type,
            });
        }

        constants
    }

    /// Try to infer a constant's type from its value.
    fn infer_constant_type(&self, value: &str) -> Option<CType> {
        let value = value.trim();

        // String literal
        if value.starts_with('"') && value.ends_with('"') {
            return Some(CType::ConstPointer(Box::new(CType::Char)));
        }

        // Character literal
        if value.starts_with('\'') && value.ends_with('\'') {
            return Some(CType::Char);
        }

        // Hex number
        if value.starts_with("0x") || value.starts_with("0X") {
            // Check for suffix
            if value.ends_with("ULL") || value.ends_with("ull") {
                return Some(CType::UInt64);
            }
            if value.ends_with("LL") || value.ends_with("ll") {
                return Some(CType::Int64);
            }
            if value.ends_with("UL") || value.ends_with("ul") {
                return Some(CType::UInt64);
            }
            if value.ends_with("L") || value.ends_with("l") {
                return Some(CType::Int64);
            }
            if value.ends_with("U") || value.ends_with("u") {
                return Some(CType::UInt32);
            }
            return Some(CType::Int32);
        }

        // Decimal number
        if value.chars().next().is_some_and(|c| c.is_ascii_digit() || c == '-') {
            // Check for float
            if value.contains('.') || value.ends_with('f') || value.ends_with('F') {
                return Some(CType::Double);
            }
            // Integer
            if value.ends_with("ULL") || value.ends_with("ull") {
                return Some(CType::UInt64);
            }
            if value.ends_with("LL") || value.ends_with("ll") {
                return Some(CType::Int64);
            }
            return Some(CType::Int32);
        }

        None
    }

    /// Check if a function should be included.
    fn should_include_function(&self, name: &str) -> bool {
        // Check exclusions first
        if self.exclude_functions.iter().any(|e| e == name) {
            return false;
        }

        // If include list is empty, include all
        if self.include_functions.is_empty() {
            return true;
        }

        // Check inclusions
        self.include_functions.iter().any(|i| i == name)
    }

    /// Check if a type should be included.
    fn should_include_type(&self, name: &str) -> bool {
        if self.exclude_types.iter().any(|e| e == name) {
            return false;
        }

        if self.include_types.is_empty() {
            return true;
        }

        self.include_types.iter().any(|i| i == name)
    }

    /// Strip prefix from a name if configured.
    fn maybe_strip_prefix(&self, name: &str) -> String {
        if let Some(ref prefix) = self.strip_prefix {
            if let Some(stripped) = name.strip_prefix(prefix) {
                return stripped.to_string();
            }
        }
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_simple_function() {
        let parser = HeaderParser::new();
        let content = "int add(int a, int b);";
        let header = parser.parse_content(content, Path::new("test.h")).unwrap();

        assert_eq!(header.functions.len(), 1);
        let func = &header.functions[0];
        assert_eq!(func.name, "add");
        assert_eq!(func.return_type, CType::Int32);
        assert_eq!(func.params.len(), 2);
        assert_eq!(func.params[0].name, "a");
        assert_eq!(func.params[1].name, "b");
    }

    #[test]
    fn test_parse_pointer_function() {
        let parser = HeaderParser::new();
        let content = "char* get_string(void);";
        let header = parser.parse_content(content, Path::new("test.h")).unwrap();

        assert_eq!(header.functions.len(), 1);
        let func = &header.functions[0];
        assert_eq!(func.name, "get_string");
        assert!(matches!(func.return_type, CType::Pointer(_)));
    }

    #[test]
    fn test_parse_struct() {
        let parser = HeaderParser::new();
        let content = "typedef struct { int x; int y; } Point;";
        let header = parser.parse_content(content, Path::new("test.h")).unwrap();

        assert_eq!(header.structs.len(), 1);
        let s = &header.structs[0];
        assert_eq!(s.name, "Point");
        assert_eq!(s.fields.len(), 2);
    }

    #[test]
    fn test_parse_enum() {
        let parser = HeaderParser::new();
        let content = "enum Color { RED = 0, GREEN = 1, BLUE = 2 };";
        let header = parser.parse_content(content, Path::new("test.h")).unwrap();

        assert_eq!(header.enums.len(), 1);
        let e = &header.enums[0];
        assert_eq!(e.name, "Color");
        assert_eq!(e.variants.len(), 3);
        assert_eq!(e.variants[0].value, Some(0));
    }

    #[test]
    fn test_strip_prefix() {
        let parser = HeaderParser::new().with_strip_prefix(Some("mylib_".to_string()));
        let content = "void mylib_init(void);";
        let header = parser.parse_content(content, Path::new("test.h")).unwrap();

        assert_eq!(header.functions[0].name, "init");
    }
}
