//! Contract hint tests: inline sources, every pattern of the module docs
//! and the negatives next to them.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{ContractKind, ContractSide, FileFacts, Language};

use crate::TypeScriptPack;

fn facts(path: &str, src: &str) -> FileFacts {
    let lang = if path.ends_with(".js") { Language::JavaScript } else { Language::TypeScript };
    TypeScriptPack.extract(path, lang, src.as_bytes(), &Budget::default()).unwrap()
}

/// `(side, kind, key, owner qualified name, framework)` of every hint.
fn hints_in(path: &str, src: &str) -> Vec<(ContractSide, ContractKind, String, String, String)> {
    let f = facts(path, src);
    f.contracts
        .iter()
        .map(|h| {
            let owner = h.owner.map(|o| f.definitions[o as usize].qualified_name.clone()).unwrap_or_default();
            (h.side, h.kind, h.key.clone(), owner, h.framework.clone())
        })
        .collect()
}

fn hints(src: &str) -> Vec<(ContractSide, ContractKind, String, String, String)> {
    hints_in("m.ts", src)
}

/// `(key, owner, framework)` of the hints of one side and kind.
fn keys(src: &str, side: ContractSide, kind: ContractKind) -> Vec<(String, String, String)> {
    hints(src).into_iter().filter(|h| h.0 == side && h.1 == kind).map(|h| (h.2, h.3, h.4)).collect()
}

fn triples(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
}

use ContractKind::{Http, Rpc};
use ContractSide::{Consumes, Provides};

#[test]
fn express_routes() {
    let src = r#"
import express from "express";
import axios from "axios";
const app = express();
const router = express.Router();

function listUsers(req, res) {}

app.get("/users", listUsers);
router.post("/users/:id", auth, async (req, res) => {});
app.all("/any", (req, res) => {});
app.route("/books/:isbn").get(listUsers).put(listUsers);
app.get("title");
app.use("/static", serve);
axios.get("/remote", { params: {} });
"#;
    assert_eq!(
        keys(src, Provides, Http),
        triples(&[
            ("GET /users", "listUsers", "express"),
            ("POST /users/{}", "", "express"),
            ("* /any", "", "express"),
            ("PUT /books/{}", "listUsers", "express"),
            ("GET /books/{}", "listUsers", "express"),
        ])
    );
    assert_eq!(keys(src, Consumes, Http), triples(&[("GET /remote", "", "axios")]));
}

#[test]
fn fastify_hapi_and_hono() {
    let src = r#"
import Fastify from "fastify";
const fastify = Fastify();
async function handler(req, reply) {}
fastify.route({ method: ["GET", "POST"], url: "/items/:id", handler });
fastify.get("/ping", async () => "pong");
"#;
    assert_eq!(
        keys(src, Provides, Http),
        triples(&[
            ("GET /items/{}", "handler", "fastify"),
            ("POST /items/{}", "handler", "fastify"),
            ("GET /ping", "", "fastify"),
        ])
    );
    let hono = r#"
import { Hono } from "hono";
const app = new Hono();
app.get("/posts/:id", (c) => c.json({}));
"#;
    assert_eq!(keys(hono, Provides, Http), triples(&[("GET /posts/{}", "", "hono")]));
    let hapi = r#"
const Hapi = require("@hapi/hapi");
server.route({ method: "DELETE", path: "/users/{id}", handler: (r, h) => null });
"#;
    assert_eq!(keys(hapi, Provides, Http), triples(&[("DELETE /users/{}", "", "hapi")]));
}

#[test]
fn routes_without_import_need_an_inline_handler() {
    let src = r#"
module.exports = function (app) {
  app.get("/inline", function (req, res) {});
  app.get("/named", handler);
  app.get("/static/*", serveStatic({ root: "./" }));
  $.get("/api/data", function (data) {});
};
"#;
    let h = hints_in("m.js", src);
    let provided: Vec<&str> = h.iter().filter(|h| h.0 == Provides).map(|h| h.2.as_str()).collect();
    let consumed: Vec<&str> = h.iter().filter(|h| h.0 == Consumes).map(|h| h.2.as_str()).collect();
    assert_eq!(provided, ["GET /inline", "GET /static/{}"]);
    assert_eq!(consumed, ["GET /named", "GET /api/data"]);
}

#[test]
fn nestjs_controllers() {
    let src = r#"
import { Controller, Get, Post, All } from "@nestjs/common";

@Controller("users")
export class UsersController {
  @Get(":id")
  findOne(@Param("id") id: string) {}

  @Post()
  @HttpCode(204)
  create() {}

  @All(["a", "b/:x"])
  any() {}

  helper() {}
}

@Controller({ path: "/admin/" })
class AdminController {
  @Get("stats")
  stats() {}
}
"#;
    assert_eq!(
        keys(src, Provides, Http),
        triples(&[
            ("GET /users/{}", "UsersController.findOne", "nestjs"),
            ("POST /users", "UsersController.create", "nestjs"),
            ("* /users/a", "UsersController.any", "nestjs"),
            ("* /users/b/{}", "UsersController.any", "nestjs"),
            ("GET /admin/stats", "AdminController.stats", "nestjs"),
        ])
    );
}

#[test]
fn http_clients() {
    let src = r#"
import axios from "axios";
import got from "got";

export async function load(id: string) {
  await fetch(`/api/users/${id}`);
  await fetch("https://api.example.com/v1/items?x=1", { method: "post" });
  await axios({ url: "/axios/form", method: "put" });
  await axios.delete("/axios/" + id);
  await got("https://h/got/path");
  const url = "/const/url";
  await fetch(url);
}

class Api {
  async list() {
    return this.request("/things/{id}", { headers: {} });
  }
  async remove() {
    return http.del(`${this.base}/things/1`);
  }
}
"#;
    assert_eq!(
        keys(src, Consumes, Http),
        triples(&[
            ("GET /api/users/{}", "load", "fetch"),
            ("POST /v1/items", "load", "fetch"),
            ("PUT /axios/form", "load", "axios"),
            ("DELETE /axios/{}", "load", "axios"),
            ("* /got/path", "load", "got"),
            ("GET /const/url", "load", "fetch"),
            ("* /things/{}", "Api.list", "js-http"),
            ("DELETE /things/1", "Api.remove", "js-http"),
        ])
    );
}

#[test]
fn one_hop_through_a_wrapper() {
    let src = r#"
export class Ollama {
  protected async processStreamableRequest<T>(endpoint: string, request: object): Promise<T> {
    const host = `${this.config.host}/api/${endpoint}`;
    return await utils.post(this.fetch, host, request, { headers: this.config.headers });
  }
  async generate(request: object) {
    return this.processStreamableRequest<object>("generate", request);
  }
  async pull(name: string) {
    return this.processStreamableRequest<object>(name, {});
  }
  async tags() {
    return await utils.get(this.fetch, `${this.config.host}/api/tags`, { headers: {} });
  }
}
"#;
    assert_eq!(
        keys(src, Consumes, Http),
        triples(&[
            ("POST /api/{}", "Ollama.processStreamableRequest", "js-http"),
            ("POST /api/generate", "Ollama.generate", "js-http"),
            ("GET /api/tags", "Ollama.tags", "js-http"),
        ])
    );
}

#[test]
fn http_client_negatives() {
    let src = r#"
const cache = new Map();
function f(url: string, key: string) {
  cache.get("name");
  map.get(key);
  fetch(url);
  fetch(`${url}`);
  axios.post("users");
  store.delete("item");
  console.log("GET /x with spaces");
}
"#;
    assert!(hints(src).is_empty(), "{:?}", hints(src));
}

#[test]
fn grpc_clients_and_services() {
    let src = r#"
import * as grpc from "@grpc/grpc-js";
import { GreeterClient } from "./proto/greeter_grpc_pb";
import { GreeterService } from "./proto/greeter_grpc_pb";
import { S3Client } from "@aws-sdk/client-s3";

const client = new GreeterClient("localhost:50051", grpc.credentials.createInsecure());
const s3 = new S3Client({});

export function greet() {
  client.sayHello(req, (err, res) => {});
  client.close();
  s3.send(cmd);
}

function sayHello(call, callback) {}

const server = new grpc.Server();
server.addService(GreeterService, { sayHello, sayGoodbye: (c, cb) => {} });
server.addService(proto.helloworld.Farewell.service, { wave() {} });
"#;
    assert_eq!(keys(src, Consumes, Rpc), triples(&[("Greeter/SayHello", "greet", "grpc-node")]));
    assert_eq!(
        keys(src, Provides, Rpc),
        triples(&[
            ("Greeter/SayHello", "sayHello", "grpc-node"),
            ("Greeter/SayGoodbye", "", "grpc-node"),
            ("Farewell/Wave", "wave", "grpc-node"),
        ])
    );
}
