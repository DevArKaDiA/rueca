//! Contract hint tests: inline sources, every pattern of the module docs
//! and the negatives next to them.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{ContractKind, ContractSide, FileFacts, Language};

use crate::PythonPack;

fn facts(src: &str) -> FileFacts {
    PythonPack.extract("m.py", Language::Python, src.as_bytes(), &Budget::default()).unwrap()
}

/// `(side, kind, key, owner qualified name, framework)` of every hint.
fn hints(src: &str) -> Vec<(ContractSide, ContractKind, String, String, String)> {
    let f = facts(src);
    f.contracts
        .iter()
        .map(|h| {
            let owner = h.owner.map(|o| f.definitions[o as usize].qualified_name.clone()).unwrap_or_default();
            (h.side, h.kind, h.key.clone(), owner, h.framework.clone())
        })
        .collect()
}

/// `(key, owner)` of the hints of one side and kind.
fn keys(src: &str, side: ContractSide, kind: ContractKind) -> Vec<(String, String)> {
    hints(src).into_iter().filter(|h| h.0 == side && h.1 == kind).map(|h| (h.2, h.3)).collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

use ContractKind::{Http, Rpc, Topic};
use ContractSide::{Consumes, Provides};

#[test]
fn flask_routes_and_blueprint_prefix() {
    let src = r#"
from flask import Flask, Blueprint
app = Flask(__name__)
bp = Blueprint("users", __name__, url_prefix="/api/")

@app.route("/health")
def health(): ...

@app.route("/items/<int:item_id>", methods=["GET", "DELETE"])
def item(item_id): ...

@bp.post("/users/<id>")
def create_user(id): ...

def legacy(): ...
app.add_url_rule("/legacy", "legacy", legacy, methods=["PUT"])
"#;
    assert_eq!(
        keys(src, Provides, Http),
        pairs(&[
            ("GET /health", "health"),
            ("GET /items/{}", "item"),
            ("DELETE /items/{}", "item"),
            ("POST /api/users/{}", "create_user"),
            ("PUT /legacy", "legacy"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "flask"));
}

#[test]
fn fastapi_router_prefix_and_methods() {
    let src = r#"
from fastapi import APIRouter, FastAPI
app = FastAPI()
router = APIRouter(prefix="/v1")

class Items:
    @router.get("/items/{item_id}")
    async def read(self, item_id: int): ...

@router.put("")
def replace(): ...

@app.api_route("/any", methods=["GET", "POST"])
def any_(): ...

@app.websocket("/ws")
def ws(): ...
"#;
    assert_eq!(
        keys(src, Provides, Http),
        pairs(&[
            ("GET /v1/items/{}", "Items.read"),
            ("PUT /v1", "replace"),
            ("GET /any", "any_"),
            ("POST /any", "any_"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "fastapi"));
}

#[test]
fn route_decorator_negatives() {
    let src = r#"
from unittest import mock
import functools

@mock.patch("pkg.module.thing")
@functools.lru_cache()
def f(): ...

@cache.get("key")
def g(): ...

@app.get(path_var)
def h(): ...
"#;
    assert!(hints(src).is_empty(), "{:?}", hints(src));
}

#[test]
fn django_aiohttp_starlette_registrations() {
    let django = r#"
from django.urls import include, path, re_path
from . import views

def local_view(request): ...

urlpatterns = [
    path("users/<int:pk>/", views.user_detail),
    path("local/", local_view),
    path("api/", include("api.urls")),
    re_path(r"^articles/(?P<year>[0-9]{4})/$", views.year_archive),
    re_path(r"^weird/[a-z]+$", views.weird),
]
"#;
    assert_eq!(
        keys(django, Provides, Http),
        pairs(&[("* /users/{}", ""), ("* /local", "local_view"), ("* /articles/{}", "")])
    );

    let aiohttp = r#"
from aiohttp import web

async def handle(request): ...

app = web.Application()
app.router.add_get("/a/{name}", handle)
app.router.add_route("POST", "/b", handle)
app.add_routes([web.get("/c", handle), web.delete("/d", handle)])
routes = web.RouteTableDef()

@routes.patch("/e")
async def patcher(request): ...
"#;
    assert_eq!(
        keys(aiohttp, Provides, Http),
        pairs(&[
            ("GET /a/{}", "handle"),
            ("POST /b", "handle"),
            ("GET /c", "handle"),
            ("DELETE /d", "handle"),
            ("PATCH /e", "patcher"),
        ])
    );
    assert!(keys(aiohttp, Consumes, Http).is_empty());

    let starlette = r#"
from starlette.routing import Route

async def homepage(request): ...

routes = [Route("/", homepage), Route("/users/{id}", endpoint=homepage, methods=["GET", "PUT"])]
"#;
    assert_eq!(
        keys(starlette, Provides, Http),
        pairs(&[("GET /", "homepage"), ("GET /users/{}", "homepage"), ("PUT /users/{}", "homepage")])
    );
}

#[test]
fn http_clients() {
    let src = r#"
import requests
import httpx

BASE = "https://api.example.com"

def fetch_user(uid):
    return requests.get(f"https://api.example.com/users/{uid}?full=1")

def create(base):
    requests.request("POST", base + "/items")
    httpx.post(url="/v2/items/" + str(1))

class Client:
    def __init__(self):
        self._client = httpx.Client()

    def ping(self, method):
        path = "/ping/%s" % method
        self._client.get(path)
        self._client.request(method, "/dyn/{name}".format(name=method))
        urllib.request.urlopen("http://localhost:8080/health")
"#;
    assert_eq!(
        keys(src, Consumes, Http),
        pairs(&[
            ("GET /users/{}", "fetch_user"),
            ("POST /items", "create"),
            ("POST /v2/items/{}", "create"),
            ("GET /ping/{}", "Client.ping"),
            ("* /dyn/{}", "Client.ping"),
            ("GET /health", "Client.ping"),
        ])
    );
    let frameworks: Vec<String> = hints(src).into_iter().map(|h| h.4).collect();
    assert_eq!(frameworks, ["requests", "requests", "httpx", "httpx", "httpx", "urllib"]);
}

#[test]
fn sdk_wrappers() {
    let src = r#"
class Ollama:
    def generate(self, model):
        return self._request(GenerateResponse, "POST", "/api/generate", json={})

class Collections:
    def update(self, name):
        return self.api_client.request(type_=m.R, method="PUT", url="/collections/{collection_name}")

class Files:
    def retrieve(self, file_id):
        return self._get(path_template("/files/{file_id}", file_id=file_id), options=x)

    def listing(self):
        return self._get_api_list("/files", page=SyncPage)

class Live:
    def is_live(self):
        return executor.execute(method=self._connection.get, path="/.well-known/live")

    def other(self):
        return executor.execute(method=self._connection.connect, path="/nothing")
"#;
    assert_eq!(
        keys(src, Consumes, Http),
        pairs(&[
            ("POST /api/generate", "Ollama.generate"),
            ("PUT /collections/{}", "Collections.update"),
            ("GET /files/{}", "Files.retrieve"),
            ("GET /files", "Files.listing"),
            ("GET /.well-known/live", "Live.is_live"),
            ("* /nothing", "Live.other"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "python-http"));
}

#[test]
fn http_client_negatives() {
    let src = r#"
import requests

def f(url, d, config):
    d.get("name")
    config.get("a.b")
    requests.get(url)
    requests.get("users")
    os.path.join("/tmp", url)
    requests.post(f"{url}")
    logger.info("GET /x with spaces")
"#;
    assert!(keys(src, Consumes, Http).is_empty(), "{:?}", hints(src));
}

#[test]
fn grpc_stubs_and_servicers() {
    let src = r#"
import grpc
from dapr.proto import api_service_v1
from qdrant_client import grpc as qgrpc
from . import greeter_pb2_grpc
from .greeter_pb2_grpc import add_GreeterServicer_to_server

class _FakeStub:
    def Call(self): ...

class DaprClient:
    def __init__(self, channel):
        self._stub = api_service_v1.DaprStub(channel)

    def invoke(self, req):
        return self._stub.InvokeService(req)

    @property
    def grpc_points(self) -> qgrpc.PointsStub:
        return self._pool[0]

    def query(self):
        return self.grpc_points.Query(x, timeout=1)

def helper(stub: api_service_v1.DaprStub):
    fake = _FakeStub()
    fake.Call()
    stub.GetState(req)
    stub.close()

class Greeter(greeter_pb2_grpc.GreeterServicer):
    def SayHello(self, request, context): ...
    def _helper(self): ...
    def helper(self): ...

def serve():
    server = grpc.server(None)
    add_GreeterServicer_to_server(Greeter(), server)
"#;
    assert_eq!(
        keys(src, Consumes, Rpc),
        pairs(&[
            ("Dapr/InvokeService", "DaprClient.invoke"),
            ("Points/Query", "DaprClient.query"),
            ("Dapr/GetState", "helper"),
        ])
    );
    assert_eq!(keys(src, Provides, Rpc), pairs(&[("Greeter/SayHello", "Greeter.SayHello")]));
    assert!(hints(src).iter().all(|h| h.4 == "grpc-python"));
}

#[test]
fn grpc_unbound_stub_holders() {
    // A stub handed in as a parameter: the service is unknown, the method
    // is not; a name bound to a non-stub class or not named `…stub` is no
    // stub.
    let src = r#"
class _FakeStub:
    def Call(self): ...

class Subscription:
    def __init__(self, stub, other):
        self._stub = stub
        self._other = other
        self._fake_stub = _FakeStub()

    def start(self):
        return self._stub.SubscribeTopicEventsAlpha1(iter([]))

    def close(self):
        self._other.Close()
        self._fake_stub.Call()
        self._stub.close()
"#;
    assert_eq!(keys(src, Consumes, Rpc), pairs(&[("*/SubscribeTopicEventsAlpha1", "Subscription.start")]));
}

#[test]
fn kafka_topics() {
    let src = r#"
from kafka import KafkaConsumer, KafkaProducer

producer = KafkaProducer(bootstrap_servers="x")

def publish(event):
    producer.send("orders", event)
    other.send("not-a-topic")

def listen():
    consumer = KafkaConsumer("payments", "refunds", group_id="g")
    consumer.subscribe(["audit"])
"#;
    assert_eq!(keys(src, Consumes, Topic), pairs(&[("orders", "publish")]));
    assert_eq!(
        keys(src, Provides, Topic),
        pairs(&[("payments", "listen"), ("refunds", "listen"), ("audit", "listen")])
    );
}

#[test]
fn raw_and_span_are_the_evidence() {
    let src = "import requests\n\ndef f():\n    requests.post(\n        \"/x\",\n        json={},\n    )\n";
    let f = facts(src);
    let h = &f.contracts[0];
    assert_eq!(h.raw, "requests.post( \"/x\", json={}, )");
    assert_eq!((h.span.start_line, h.span.end_line), (3, 6));
}

#[test]
fn django_regex_paths() {
    use super::routes::regex_path;
    assert_eq!(regex_path(r"^a/(?P<id>\d+)/b/$").as_deref(), Some("a/{p}/b/"));
    assert_eq!(regex_path(r"^a/(x(y))$").as_deref(), Some("a/{p}"));
    assert_eq!(regex_path(r"^a/\d+$"), None);
    assert_eq!(regex_path(r"^a/(x$"), None);
}
