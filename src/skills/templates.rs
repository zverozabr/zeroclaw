//! Skill scaffolding templates.
//!
//! Provides pre-built templates for creating new skills in various languages.

/// A single file in a skill template.
pub struct TemplateFile {
    pub path: &'static str,
    pub content: &'static str,
}

/// A skill template definition.
pub struct Template {
    pub name: &'static str,
    pub language: &'static str,
    pub description: &'static str,
    pub files: &'static [TemplateFile],
    pub test_args: &'static str,
}

/// All available skill templates.
pub static ALL: &[Template] = &[
    Template {
        name: "typescript",
        language: "TypeScript",
        description: "TypeScript skill with Node.js runtime",
        files: &[
            TemplateFile {
                path: "SKILL.toml",
                content: r#"[skill]
name = "{{NAME}}"
version = "0.1.0"
description = "A {{NAME}} skill"

[[tools]]
name = "{{NAME}}"
description = "Run {{NAME}}"
command = "npx tsx src/main.ts"

[tools.args]
input = { type = "string", description = "Input text", required = true }
"#,
            },
            TemplateFile {
                path: "src/main.ts",
                content: r#"const args = JSON.parse(process.argv[2] || "{}");
const input = args.input || "";

console.log(JSON.stringify({ success: true, output: `Hello from {{NAME}}: ${input}` }));
"#,
            },
            TemplateFile {
                path: "package.json",
                content: r#"{
  "name": "{{NAME}}",
  "version": "0.1.0",
  "private": true,
  "dependencies": {},
  "devDependencies": {
    "tsx": "^4.0.0",
    "typescript": "^5.0.0"
  }
}
"#,
            },
            TemplateFile {
                path: "tsconfig.json",
                content: r#"{
  "compilerOptions": {
    "target": "ES2020",
    "module": "commonjs",
    "strict": true,
    "outDir": "./dist"
  },
  "include": ["src"]
}
"#,
            },
        ],
        test_args: r#"{"input": "test"}"#,
    },
    Template {
        name: "python",
        language: "Python",
        description: "Python skill script",
        files: &[
            TemplateFile {
                path: "SKILL.toml",
                content: r#"[skill]
name = "{{NAME}}"
version = "0.1.0"
description = "A {{NAME}} skill"

[[tools]]
name = "{{NAME}}"
description = "Run {{NAME}}"
command = "python3 main.py"

[tools.args]
input = { type = "string", description = "Input text", required = true }
"#,
            },
            TemplateFile {
                path: "main.py",
                content: r#"#!/usr/bin/env python3
import json
import sys

args = json.loads(sys.argv[1]) if len(sys.argv) > 1 else {}
input_text = args.get("input", "")

print(json.dumps({"success": True, "output": f"Hello from {{NAME}}: {input_text}"}))
"#,
            },
            TemplateFile {
                path: "requirements.txt",
                content: "# Add dependencies here\n",
            },
        ],
        test_args: r#"{"input": "test"}"#,
    },
    Template {
        name: "rust",
        language: "Rust",
        description: "Rust skill compiled to native binary",
        files: &[
            TemplateFile {
                path: "SKILL.toml",
                content: r#"[skill]
name = "{{NAME}}"
version = "0.1.0"
description = "A {{NAME}} skill"

[[tools]]
name = "{{NAME}}"
description = "Run {{NAME}}"
command = "./target/release/{{BIN_NAME}}"

[tools.args]
input = { type = "string", description = "Input text", required = true }
"#,
            },
            TemplateFile {
                path: "Cargo.toml",
                content: r#"[package]
name = "{{BIN_NAME}}"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
serde_json = "1"
"#,
            },
            TemplateFile {
                path: "src/main.rs",
                content: r#"use serde_json::{json, Value};
use std::env;

fn main() {
    let args: Value = env::args()
        .nth(1)
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(json!({}));

    let input = args["input"].as_str().unwrap_or("");
    let result = json!({
        "success": true,
        "output": format!("Hello from {{NAME}}: {input}")
    });
    println!("{}", result);
}
"#,
            },
        ],
        test_args: r#"{"input": "test"}"#,
    },
    Template {
        name: "go",
        language: "Go",
        description: "Go skill compiled to native binary",
        files: &[
            TemplateFile {
                path: "SKILL.toml",
                content: r#"[skill]
name = "{{NAME}}"
version = "0.1.0"
description = "A {{NAME}} skill"

[[tools]]
name = "{{NAME}}"
description = "Run {{NAME}}"
command = "./{{BIN_NAME}}"

[tools.args]
input = { type = "string", description = "Input text", required = true }
"#,
            },
            TemplateFile {
                path: "main.go",
                content: r#"package main

import (
	"encoding/json"
	"fmt"
	"os"
)

func main() {
	args := map[string]interface{}{}
	if len(os.Args) > 1 {
		json.Unmarshal([]byte(os.Args[1]), &args)
	}

	input, _ := args["input"].(string)
	result := map[string]interface{}{
		"success": true,
		"output":  fmt.Sprintf("Hello from {{NAME}}: %s", input),
	}
	out, _ := json.Marshal(result)
	fmt.Println(string(out))
}
"#,
            },
            TemplateFile {
                path: "go.mod",
                content: "module {{BIN_NAME}}\n\ngo 1.21\n",
            },
        ],
        test_args: r#"{"input": "test"}"#,
    },
];

/// Find a template by name (case-insensitive).
pub fn find(name: &str) -> Option<&'static Template> {
    ALL.iter().find(|t| t.name.eq_ignore_ascii_case(name))
}

/// Apply placeholder substitution to template content.
pub fn apply(content: &str, name: &str, bin_name: &str) -> String {
    content
        .replace("{{NAME}}", name)
        .replace("{{BIN_NAME}}", bin_name)
}
