use code_graph_mcp::parser::treesitter::parse_code;
use code_graph_mcp::parser::relations::extract_relations;

#[test]
fn test_java_inheritance_parsing() {
    let code = "public class Dog extends Animal { public void bark() {} }";
    let nodes = parse_code(code, "java").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"Dog"), "Java class should be parsed, got: {:?}", names);
    assert!(names.contains(&"bark"), "Java method should be parsed, got: {:?}", names);

    let relations = extract_relations(code, "java").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "inherits")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"Animal"), "Java inheritance should be extracted, got: {:?}", inherits);
}

#[test]
fn test_typescript_class_parsing() {
    let code = r#"
class UserService {
    private db: Database;

    async getUser(id: string): Promise<User> {
        return this.db.find(id);
    }

    async deleteUser(id: string): Promise<void> {
        await this.db.delete(id);
    }
}
"#;
    let nodes = parse_code(code, "typescript").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserService"), "TS class should be parsed, got: {:?}", names);
    assert!(names.contains(&"getUser"), "TS method should be parsed, got: {:?}", names);
    assert!(names.contains(&"deleteUser"), "TS method should be parsed, got: {:?}", names);
}

#[test]
fn test_python_function_and_class_parsing() {
    let code = r#"
class Animal:
    def __init__(self, name):
        self.name = name

    def speak(self):
        pass

def create_animal(name):
    return Animal(name)
"#;
    let nodes = parse_code(code, "python").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"Animal"), "Python class should be parsed, got: {:?}", names);
    assert!(names.contains(&"create_animal"), "Python function should be parsed, got: {:?}", names);
    assert!(!nodes.is_empty(), "Should produce nodes from Python code");
}

#[test]
fn test_go_function_parsing() {
    let code = r#"
package main

import "fmt"

func greet(name string) string {
    return fmt.Sprintf("Hello, %s", name)
}

func main() {
    greet("world")
}
"#;
    let nodes = parse_code(code, "go").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"greet"), "Go function should be parsed, got: {:?}", names);
    assert!(names.contains(&"main"), "Go main should be parsed, got: {:?}", names);
}

#[test]
fn test_rust_function_parsing() {
    let code = r#"
struct Config {
    host: String,
    port: u16,
}

fn create_config() -> Config {
    Config { host: "localhost".into(), port: 8080 }
}

impl Config {
    fn address(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
}
"#;
    let nodes = parse_code(code, "rust").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"Config"), "Rust struct should be parsed, got: {:?}", names);
    assert!(names.contains(&"create_config"), "Rust function should be parsed, got: {:?}", names);
}

#[test]
fn test_c_function_parsing() {
    let code = r#"
#include <stdio.h>

int add(int a, int b) {
    return a + b;
}

int main() {
    printf("%d\n", add(1, 2));
    return 0;
}
"#;
    let nodes = parse_code(code, "c").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"add"), "C function should be parsed, got: {:?}", names);
    assert!(names.contains(&"main"), "C main should be parsed, got: {:?}", names);
}

#[test]
fn test_typescript_call_relations() {
    let code = r#"
function validateInput(data: string): boolean {
    return data.length > 0;
}

function processRequest(req: Request) {
    if (validateInput(req.body)) {
        saveToDatabase(req.body);
        sendNotification(req.userId);
    }
}
"#;
    let relations = extract_relations(code, "typescript").unwrap();
    let calls: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "calls")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(calls.contains(&"validateInput"), "Should extract call to validateInput, got: {:?}", calls);
    assert!(calls.contains(&"saveToDatabase"), "Should extract call to saveToDatabase, got: {:?}", calls);
    assert!(calls.contains(&"sendNotification"), "Should extract call to sendNotification, got: {:?}", calls);
}

#[test]
fn test_python_import_relations() {
    let code = r#"
import os
from pathlib import Path
from collections import OrderedDict, defaultdict

def process():
    path = Path(os.getcwd())
    return path
"#;
    let relations = extract_relations(code, "python").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"os"), "Should extract 'import os', got: {:?}", imports);
    assert!(imports.contains(&"Path"), "Should extract 'from pathlib import Path', got: {:?}", imports);
    assert!(imports.contains(&"OrderedDict"), "Should extract OrderedDict import, got: {:?}", imports);
    assert!(imports.contains(&"defaultdict"), "Should extract defaultdict import, got: {:?}", imports);
}

#[test]
fn test_csharp_parsing() {
    let code = r#"
using System;

public class UserService : IUserService {
    public void GetUser(int id) {
        Console.WriteLine(id);
    }
    private void Helper() {}
}

public interface IUserService {
    void GetUser(int id);
}
"#;
    let nodes = parse_code(code, "csharp").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserService"), "C# class should be parsed, got: {:?}", names);
    assert!(names.contains(&"GetUser"), "C# method should be parsed, got: {:?}", names);
    assert!(names.contains(&"IUserService"), "C# interface should be parsed, got: {:?}", names);

    let relations = extract_relations(code, "csharp").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(), "C# using should create imports, got: {:?}", imports);
}

#[test]
fn test_kotlin_parsing() {
    let code = r#"
import kotlinx.coroutines.flow.Flow

class UserService {
    fun findById(id: Long): String {
        return id.toString()
    }
    fun listAll(): List<String> = emptyList()
}

interface UserRepository {
    fun findById(id: Long): String
}
"#;
    let nodes = parse_code(code, "kotlin").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserService"), "Kotlin class, got: {:?}", names);
    assert!(names.contains(&"findById"), "Kotlin fun, got: {:?}", names);
    assert!(names.contains(&"UserRepository"), "Kotlin interface, got: {:?}", names);

    // Verify interface is typed correctly
    let repo = nodes.iter().find(|n| n.name == "UserRepository").unwrap();
    assert_eq!(repo.node_type, "interface", "Kotlin interface should have type 'interface', got: {:?}", repo.node_type);

    let relations = extract_relations(code, "kotlin").unwrap();
    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(!imports.is_empty(), "Kotlin imports should be extracted, got: {:?}", imports);
    assert!(imports.contains(&"Flow"), "Should import Flow, got: {:?}", imports);
}

#[test]
fn test_ruby_parsing() {
    let code = r#"
require 'json'

class UserController < ApplicationController
  def index
    users = User.all
    render json: users
  end

  def show
    user = User.find(params[:id])
  end
end

module Helpers
  def format_name(user)
    user.name
  end
end
"#;
    let nodes = parse_code(code, "ruby").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserController"), "Ruby class, got: {:?}", names);
    assert!(names.contains(&"index"), "Ruby method, got: {:?}", names);
    assert!(names.contains(&"Helpers"), "Ruby module, got: {:?}", names);

    let relations = extract_relations(code, "ruby").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "inherits")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"ApplicationController"), "Ruby inheritance, got: {:?}", inherits);
}

#[test]
fn test_php_parsing() {
    let code = r#"<?php
use App\Models\User;

class UserController extends Controller {
    public function index() {
        return User::all();
    }
    public function show(int $id) {
        return User::findOrFail($id);
    }
}

interface Authenticatable {
    public function getAuthIdentifier();
}
"#;
    let nodes = parse_code(code, "php").unwrap();
    let names: Vec<&str> = nodes.iter().map(|n| n.name.as_str()).collect();
    assert!(names.contains(&"UserController"), "PHP class, got: {:?}", names);
    assert!(names.contains(&"index"), "PHP method, got: {:?}", names);
    assert!(names.contains(&"show"), "PHP method, got: {:?}", names);
    assert!(names.contains(&"Authenticatable"), "PHP interface, got: {:?}", names);
    assert!(names.contains(&"getAuthIdentifier"), "PHP interface method, got: {:?}", names);

    let relations = extract_relations(code, "php").unwrap();
    let inherits: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "inherits")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(inherits.contains(&"Controller"), "PHP extends, got: {:?}", inherits);

    let imports: Vec<&str> = relations.iter()
        .filter(|r| r.relation == "imports")
        .map(|r| r.target_name.as_str())
        .collect();
    assert!(imports.contains(&"User"), "PHP use import, got: {:?}", imports);
}
