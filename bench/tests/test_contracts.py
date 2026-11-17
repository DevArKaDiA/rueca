"""Cross-service edges (S2, contracts.py): key canonicalization, the
independent labelers on inline fixtures, gold pairing and the scorer."""

from pathlib import Path

import pytest

from rueca_bench.contracts import GROUPS, _entries, audit_sample, classify, edge_tokens, pooled, score_group, score_record, scorecard_block
from rueca_bench.contracts_gold import build_gold, label_sites
from rueca_bench.contracts_gold.golang import GoRepo, gin_routes, go_grpc_calls, go_grpc_impls, implements, nethttp_routes
from rueca_bench.contracts_gold.keys import canonical_http, match_http, match_rpc
from rueca_bench.contracts_gold.model import CALL, HANDLER, IMPL, SPEC, Site, in_scope, symbol_tokens
from rueca_bench.contracts_gold.pairs import build_pairs
from rueca_bench.contracts_gold.pyclient import python_http_calls, python_servicers, python_stub_calls
from rueca_bench.contracts_gold.rust import actix_routes, snake, tonic_impls
from rueca_bench.contracts_gold.specs import ProtoSet, openapi_operations, proto_services
from rueca_bench.contracts_gold.tsclient import ts_http_calls
from rueca_bench.report import splice_s2, with_contracts


def write(root: Path, rel: str, text: str) -> None:
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text)


# --- keys ------------------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("method", "path", "key"),
    [
        ("get", "https://api.example.com:8443/users/:id/?x=1#frag", "GET /users/{}"),
        ("POST", "/users/{id}/", "POST /users/{}"),
        ("GET", "/files/<int:id>", "GET /files/{}"),
        ("GET", "/blog/[slug]", "GET /blog/{}"),
        ("GET", "/a/${x}/b", "GET /a/{}/b"),
        ("GET", "/a/{}", "GET /a/{}"),
        ("GET", "/items/%s/%d", "GET /items/{}/{}"),
        ("GET", "/static/*", "GET /static/{}"),
        ("GET", "/static/*path", "GET /static/{}"),
        ("GET", "/static/{rest:*}", "GET /static/{}"),
        ("GET", "/api/blobs/sha256:{digest}", "GET /api/blobs/{}"),
        ("GET", "${this.config.host}/api/tags", "GET /api/tags"),
        ("GET", "{base_url}/api/tags", "GET /api/tags"),
        ("GET", "/users/42", "GET /users/42"),
        ("GET", "", "GET /"),
        ("GET", "https://ollama.com", "GET /"),
        (None, "/x", "* /x"),
        ("fetch", "/x", "* /x"),
        ("get", "//double//slash/", "GET /double/slash"),
    ],
)
def test_canonical_http(method, path, key):
    assert canonical_http(method, path) == key


def test_canonical_http_is_case_sensitive():
    assert canonical_http("GET", "/Users") != canonical_http("GET", "/users")


def test_match_http_rules():
    assert match_http("GET /users/{}", "GET /users/{}") == "exact"
    assert match_http("GET /users/{}", "GET /users/42") == "template"
    assert match_http("GET /users/42", "GET /users/{}") is None, "a consumer hole does not match a provider literal"
    assert match_http("GET /users/{}", "GET /users/42/posts") is None, "same segment count"
    assert match_http("* /users/{}", "POST /users/1") == "method-wildcard"
    assert match_http("GET /users", "* /users") == "method-wildcard"
    assert match_http("GET /users", "POST /users") is None
    assert match_http("GET /", "GET /") == "exact"


def test_match_rpc_rules():
    assert match_rpc("pkg.v1.Svc/Get", "pkg.v1.Svc/Get") == "exact"
    assert match_rpc("pkg.v1.Svc/Get", "Svc/Get") == "template"
    assert match_rpc("Svc/Get", "pkg.v1.Svc/Get") == "template", "package-less matches either way"
    assert match_rpc("pkg.v0.Svc/Get", "pkg.v1.Svc/Get") is None
    assert match_rpc("pkg.v1.Svc/Get", "Svc/Put") is None
    assert match_rpc("pkg.v1.Other/Get", "Svc/Get") is None


def test_scope_and_symbol_tokens():
    assert in_scope("ollama/_client.py")
    assert not in_scope("tests/test_client.py") and not in_scope("server/routes_test.go") and not in_scope("src/a.test.ts")
    assert not in_scope("dapr/proto/runtime/v1/dapr_pb2_grpc.py") and not in_scope("pkg/proto/runtime/v1/dapr.pb.go")
    assert not in_scope(None)
    assert symbol_tokens("server.Server.GenerateHandler") == ("GenerateHandler", "Server.GenerateHandler")
    assert symbol_tokens("crate::api::cluster_api::cluster_status") == ("cluster_status", "cluster_api.cluster_status")
    assert symbol_tokens("") == ("", "")


# --- labelers --------------------------------------------------------------------------------


PY_CLIENT = '''
import httpx

class Client:
    def generate(self):
        return self._request(Resp, 'POST', '/api/generate', json={})

    def blob(self, digest):
        self._request_raw('POST', f'/api/blobs/{digest}', content=b'')

    def files(self, file_id):
        return self._get(path_template("/files/{file_id}", file_id=file_id))

    def roles(self):
        path = "/authz/roles"
        return executor.execute(method=self._connection.get, path=path)

    def remote(self):
        return httpx.get("https://xkcd.com/info.0.json")

    def nodes(self):
        return executor.result(self._connection.get(path="/nodes"))

    def not_http(self, d):
        return d.get("name")

@app.get("/health")
def health():
    return 1
'''


def test_python_http_labeler(tmp_path):
    write(tmp_path, "pkg/client.py", PY_CLIENT)
    write(tmp_path, "tests/test_client.py", "def test(c):\n    c._request(R, 'GET', '/api/tags')\n")
    sites = python_http_calls("o/py", tmp_path)
    got = {(s.key, s.symbol) for s in sites}
    assert got == {
        ("POST /api/generate", "Client.generate"),
        ("POST /api/blobs/{}", "Client.blob"),
        ("GET /files/{}", "Client.files"),
        ("GET /authz/roles", "Client.roles"),
        ("GET /info.0.json", "Client.remote"),
        ("GET /nodes", "Client.nodes"),
    }, "decorated routes, dict.get and test files are not client calls"
    assert all(s.role == CALL and s.path == "pkg/client.py" for s in sites)


TS_CLIENT = """
export class Ollama {
  async processStreamableRequest(endpoint: string, request: any) {
    const host = `${this.config.host}/api/${endpoint}`
    return utils.post(this.fetch, host, request)
  }
  async generate(request: any) {
    return this.processStreamableRequest('generate', request)
  }
  async remove(request: any) {
    await utils.del(this.fetch, `${this.config.host}/api/delete`, { name: request.model })
  }
  async raw(id: string) {
    return fetch('/v1/items/' + id, { method: 'PUT' })
  }
}
"""


def test_ts_http_labeler_with_one_hop(tmp_path):
    write(tmp_path, "src/browser.ts", TS_CLIENT)
    keys = {(s.key, s.symbol, s.framework) for s in ts_http_calls("o/js", tmp_path)}
    assert ("POST /api/generate", "Ollama.generate", "ts-http via processStreamableRequest") in keys
    assert ("POST /api/{}", "Ollama.processStreamableRequest", "ts-http") in keys, "the template site itself"
    assert ("DELETE /api/delete", "Ollama.remove", "ts-http") in keys
    assert ("PUT /v1/items/{}", "Ollama.raw", "ts-http") in keys


GO_ROUTES = """package server

import "github.com/gin-gonic/gin"

func (s *Server) GenerateRoutes() {
	r := gin.Default()
	r.GET("/api/version", func(c *gin.Context) { c.JSON(200, gin.H{"version": version.Version}) })
	r.POST("/api/chat", s.withLogging("/api/chat", s.ChatHandler)...)
	r.POST("/v1/chat/completions",
		middleware.ChatMiddleware(),
		s.ChatHandler,
	)
	v2 := r.Group("/v2")
	v2.DELETE("/models/:name", s.DeleteHandler)
	r.Any("/proxy/*path", s.Missing)
}

func (s *Server) ChatHandler(c *gin.Context) {}
"""


def test_go_gin_routes_and_handlers(tmp_path):
    write(tmp_path, "server/routes.go", GO_ROUTES)
    write(tmp_path, "server/delete.go", "package server\n\nfunc (s *Server) DeleteHandler(c *gin.Context) {}\n")
    sites, unresolved = gin_routes(GoRepo("o/go", tmp_path))
    got = {(s.key, s.path, s.symbol, s.accept) for s in sites}
    assert ("GET /api/version", "server/routes.go", "Server.GenerateRoutes", ("GenerateRoutes", "")) in got, "inline closure: the enclosing function"
    assert ("POST /api/chat", "server/routes.go", "Server.ChatHandler", ("ChatHandler",)) in got, "last function reference, not the middleware"
    assert ("POST /v1/chat/completions", "server/routes.go", "Server.ChatHandler", ("ChatHandler",)) in got, "arguments over several lines"
    assert ("DELETE /v2/models/{}", "server/delete.go", "Server.DeleteHandler", ("DeleteHandler",)) in got, "group prefix, handler in another file"
    assert [u["key"] for u in unresolved] == ["* /proxy/{}"]


GO_MUX = """package ui

import "net/http"

func (s *Server) Handler() http.Handler {
	mux := http.NewServeMux()
	proxy := s.ollamaProxy()
	mux.Handle("GET /api/tags", proxy)
	mux.HandleFunc("POST /api/v1/chat/{id}", s.chat)
	mux.HandleFunc("/healthz", func(w http.ResponseWriter, r *http.Request) {})
	mux.Handle("GET /", s.unknown())
	r.Handle("GET", "/gin/style", h)
	return mux
}

func (s *Server) chat(w http.ResponseWriter, r *http.Request) {}
func (s *Server) proxy() {}
"""


def test_go_nethttp_routes(tmp_path):
    write(tmp_path, "app/ui/ui.go", GO_MUX)
    sites, unresolved = nethttp_routes(GoRepo("o/go", tmp_path))
    got = {(s.key, s.symbol, s.accept) for s in sites}
    assert got == {
        ("GET /api/tags", "Server.proxy", ("proxy",)),
        ("POST /api/v1/chat/{}", "Server.chat", ("chat",)),
        ("* /healthz", "Server.Handler", ("Handler", "")),
    }, "method patterns, unexported handlers, inline closures; gin's two-string Handle is not a mux pattern"
    assert [u["key"] for u in unresolved] == ["GET /"]


PROTO = """syntax = "proto3";
package acme.v1;
option go_package = "github.com/acme/api/gen/v1;acmev1";
// service Hidden { rpc Nope(A) returns (B); }
service Store {
  rpc Get(GetRequest) returns (GetResponse) {}
  rpc Watch(stream WatchRequest) returns (stream WatchResponse);
  rpc Ping(google.protobuf.Empty) returns (google.protobuf.Empty);
}
"""


def _protos(root: Path) -> ProtoSet:
    write(root, "proto/store.proto", PROTO)
    ps = ProtoSet()
    proto_services("acme/api", root, ps)
    return ps


def test_proto_reader(tmp_path):
    ps = _protos(tmp_path)
    assert [(s.key, s.symbol, s.accept) for s in ps.sites] == [
        ("acme.v1.Store/Get", "acme.v1.Store.Get", ("Store.Get",)),
        ("acme.v1.Store/Watch", "acme.v1.Store.Watch", ("Store.Watch",)),
        ("acme.v1.Store/Ping", "acme.v1.Store.Ping", ("Store.Ping",)),
    ], "commented-out services are ignored"
    assert ps.rpcs[("acme.v1", "Store")]["Watch"] == ("WatchRequest", "WatchResponse")
    assert ps.go_package[("acme.v1", "Store")] == ("github.com/acme/api/gen/v1", "acmev1")
    assert ps.services == {"Store": {"Get", "Watch", "Ping"}}


GO_SERVER = """package api

import (
	"context"

	pb "github.com/acme/api/gen/v1"
	other "github.com/acme/other/v1"
)

type server struct {
	pb.UnimplementedStoreServer
}

func Register(s *grpc.Server) { pb.RegisterStoreServer(s, &server{}) }

func (s *server) Get(ctx context.Context, in *pb.GetRequest) (*pb.GetResponse, error) { return nil, nil }
func (s *server) Watch(stream pb.Store_WatchServer) error { return nil }
func (s *server) Ping(ctx context.Context, in *emptypb.Empty) (*emptypb.Empty, error) { return nil, nil }
func (h *helper) Get(ctx context.Context, in *pb.GetRequest) (*pb.GetResponse, error) { return nil, nil }
func (o *otherServer) Get(ctx context.Context, in *other.GetRequest) (*other.GetResponse, error) { return nil, nil }
"""

GO_CLIENT = """package client

import pb "github.com/acme/api/gen/v1"

type Client struct {
	protoClient pb.StoreClient
}

func (c *Client) Fetch(ctx context.Context) {
	c.protoClient.Get(ctx, &pb.GetRequest{})
	pb.NewStoreClient(conn).Ping(ctx, nil)
	c.protoClient.Close()
}
"""


def test_go_grpc_impls_and_calls(tmp_path):
    ps = _protos(tmp_path / "api")
    write(tmp_path / "srv", "api/server.go", GO_SERVER)
    impls = go_grpc_impls(GoRepo("acme/srv", tmp_path / "srv"), ps)
    assert sorted((s.key, s.symbol) for s in impls) == [
        ("acme.v1.Store/Get", "server.Get"),
        ("acme.v1.Store/Ping", "server.Ping"),
        ("acme.v1.Store/Watch", "server.Watch"),
    ], "only the declared server type; another package's same-named rpc is not an impl"
    write(tmp_path / "cli", "client/client.go", GO_CLIENT)
    calls = go_grpc_calls(GoRepo("acme/cli", tmp_path / "cli"), ps)
    assert sorted((s.key, s.symbol) for s in calls) == [("acme.v1.Store/Get", "Client.Fetch"), ("acme.v1.Store/Ping", "Client.Fetch")]


def test_go_implements_rejects_client_signatures():
    sig = "func (c *storeClient) Get(ctx context.Context, in *GetRequest, opts ...grpc.CallOption) (*GetResponse, error) {"
    assert not implements(sig, "Store", "Get", "GetRequest", "GetResponse")
    assert implements("func (s *x) Get(ctx context.Context, in *pb.GetRequest) (*pb.GetResponse, error) {", "Store", "Get", "GetRequest", "GetResponse")


PY_GRPC = """
from acme.gen import store_pb2_grpc as api

class Client:
    def __init__(self, channel):
        self._stub = api.StoreStub(channel)

    def get(self):
        return self._stub.Get.with_call(req)

    @property
    def store(self) -> api.StoreStub:
        return self._stub

    def ping(self):
        return self.store.Ping(req)

class Servicer(api.StoreServicer):
    def Get(self, request, context):
        return None

    def helper(self):
        return None
"""


def test_python_grpc_labelers(tmp_path):
    ps = _protos(tmp_path / "api")
    root = tmp_path / "py"
    write(root, "acme/gen/store_pb2_grpc.py", "channel.unary_unary('/acme.v1.Store/Get')\n")
    write(root, "acme/client.py", PY_GRPC)
    calls = python_stub_calls("acme/py", root, ps.services)
    assert sorted((s.key, s.symbol) for s in calls) == [("acme.v1.Store/Get", "Client.get"), ("acme.v1.Store/Ping", "Client.ping")]
    impls = python_servicers("acme/py", root, ps.services)
    assert [(s.key, s.symbol, s.role) for s in impls] == [("acme.v1.Store/Get", "Servicer.Get", IMPL)]


def test_python_servicer_through_an_alias_module(tmp_path):
    ps = _protos(tmp_path / "api")
    root = tmp_path / "py"
    write(root, "acme/gen/store_pb2_grpc.py", "channel.unary_unary('/acme.v1.Store/Get')\n")
    write(root, "acme/ext/servicer.py", "from acme import store_service_v1\n\nclass S(store_service_v1.StoreServicer):\n    def Get(self, request, context):\n        return None\n")
    impls = python_servicers("acme/py", root, ps.services)
    assert [(s.key, s.symbol) for s in impls] == [("acme.v1.Store/Get", "S.Get")], "the only generated module declaring the service names its package"


def test_rust_labelers(tmp_path):
    write(tmp_path, "src/api.rs", '#[get("/collections/{name}")]\nasync fn get_collection(path: web::Path<String>) -> impl Responder {}\n')
    write(tmp_path, "src/svc.rs", "#[tonic::async_trait]\nimpl Store for StoreService {\n    async fn get(&self, r: Request<GetRequest>) -> R {\n        inner();\n    }\n    async fn helper(&self) {}\n}\n")
    routes = actix_routes("q/q", tmp_path)
    assert [(s.key, s.symbol, s.role) for s in routes] == [("GET /collections/{}", "get_collection", HANDLER)]
    write(tmp_path, "proto/store.proto", PROTO)
    ps = ProtoSet()
    proto_services("q/q", tmp_path, ps)
    assert [(s.key, s.symbol) for s in tonic_impls("q/q", tmp_path, ps)] == [("acme.v1.Store/Get", "StoreService.get")]
    write(tmp_path, "src/wrap.rs", "impl<T: Store> Store for Wrapper<T> {\n    async fn get(&self, r: Request<GetRequest>) -> R {\n        self.inner.get(r).await\n    }\n}\n")
    got = sorted((s.key, s.symbol) for s in tonic_impls("q/q", tmp_path, ps))
    assert got == [("acme.v1.Store/Get", "StoreService.get"), ("acme.v1.Store/Get", "Wrapper.get")], "a generic delegating wrapper serves the service too"
    assert snake("QueryBatch") == "query_batch" and snake("HealthCheck") == "health_check"


def test_openapi_reader(tmp_path):
    write(tmp_path, "docs/openapi.yaml", "openapi: 3.1.0\ninfo: {title: t, version: '1'}\npaths:\n  /api/blobs/{digest}:\n    head: {}\n    post: {}\n    parameters: []\n")
    write(tmp_path, "schema.json", '{"paths": {"/schema": {"get": {}}}, "swagger": "2.0"}')
    write(tmp_path, "package.json", '{"name": "x"}')
    sites, docs = openapi_operations("o/o", tmp_path)
    assert sorted((s.path, s.key, s.symbol) for s in sites) == [
        ("docs/openapi.yaml", "HEAD /api/blobs/{}", "HEAD /api/blobs/{digest}"),
        ("docs/openapi.yaml", "POST /api/blobs/{}", "POST /api/blobs/{digest}"),
        ("schema.json", "GET /schema", "GET /schema"),
    ]
    assert all(s.role == SPEC and s.accept == (s.key,) for s in sites)
    assert {d["path"]: d["operations"] for d in docs} == {"docs/openapi.yaml": 2, "schema.json": 1}


# --- pairs -----------------------------------------------------------------------------------


def _site(kind, role, key, repo, path, symbol="", accept=()):
    return Site(kind, role, key, repo, path, symbol, 1, "raw", "t", accept)


def test_build_pairs_best_quality_cross_repo_only():
    sites = [
        _site("http", CALL, "GET /users/me", "c/cli", "cli.py", "Cli.me"),
        _site("http", HANDLER, "GET /users/{}", "s/srv", "users.go", "Server.User", ("User",)),
        _site("http", HANDLER, "GET /users/me", "s/srv", "me.go", "Server.Me", ("Me",)),
        _site("http", SPEC, "GET /users/{}", "s/srv", "api.yaml", "GET /users/{id}", ("GET /users/{}",)),
        _site("http", CALL, "GET /local", "s/srv", "self.py"),
        _site("http", HANDLER, "GET /local", "s/srv", "local.go", "L", ("L",)),
        _site("http", CALL, "GET /nowhere", "c/cli", "cli.py"),
        _site("rpc", IMPL, "acme.v1.Store/Get", "c/cli", "server.go", "server.Get", ("Get",)),
        _site("rpc", SPEC, "acme.v1.Store/Get", "s/srv", "store.proto", "acme.v1.Store.Get", ("Store.Get",)),
    ]
    res = build_pairs(sites)
    got = {(p.kind, p.src_path, p.dst_path, p.dst_symbol, p.quality) for p in res["pairs"]}
    assert got == {
        ("http-code", "cli.py", "me.go", "Server.Me", "exact"),  # the static route wins over the template
        ("http-spec", "cli.py", "api.yaml", "GET /users/{id}", "template"),
        ("rpc-spec-server", "server.go", "store.proto", "acme.v1.Store.Get", "exact"),
    }
    assert res["intra"] == {"http-code": 1}
    assert [d.key for d in res["dangling"]] == ["GET /nowhere"]
    impl = next(p for p in res["pairs"] if p.kind == "rpc-spec-server")
    assert impl.src_accept == ("Get",) and impl.id[3] == "server.Get"


def test_build_gold_end_to_end(tmp_path):
    write(tmp_path / "srv", "server/routes.go", GO_ROUTES)
    write(tmp_path / "srv", "docs/openapi.yaml", "openapi: 3.0.0\npaths:\n  /api/chat:\n    post: {}\n")
    write(tmp_path / "py", "pkg/client.py", PY_CLIENT.replace("/api/generate", "/api/chat"))
    gold = build_gold({"o/srv": tmp_path / "srv", "o/py": tmp_path / "py"}, ("http-code", "http-spec"))
    assert gold["counts"] == {"http-code": 1, "http-spec": 1}
    sites, _ = label_sites({"o/srv": tmp_path / "srv"})
    assert any(s.role == HANDLER for s in sites)


# --- scoring ---------------------------------------------------------------------------------


def _edge(src_repo, src_path, src, dst_repo, dst_path, dst, dst_label, kind="CALLS", strategy="contract"):
    return {"src_repo": src_repo, "src_path": src_path, "src": src, "dst_repo": dst_repo, "dst_path": dst_path, "dst": dst, "dst_label": dst_label, "kind": kind, "strategy": strategy, "confidence": 0.9}


def test_edge_tokens_and_classify():
    route = _edge("c", "cli.py", "Cli.me", "s", "api.yaml", "api.yaml:paths./users/{id}.get", "Route")
    assert "GET /users/{}" in edge_tokens(route)
    assert classify(route) == "http-spec"
    rpc = _edge("c", "cli.py", "Cli.get", "s", "store.proto", "acme.v1.Store.Get", "RpcMethod")
    assert "Store.Get" in edge_tokens(rpc) and classify(rpc) == "rpc-spec-client"
    assert classify(dict(rpc, kind="IMPLEMENTS")) == "rpc-spec-server"
    code = _edge("c", "cli.py", "Cli.me", "s", "me.go", "server.Server.Me", "Method")
    assert classify(code, "http") == "http-code" and classify(code, "rpc") == "rpc-code"
    assert classify(code, None, ("http-code", "http-spec")) == "http-code"
    assert classify(code, None, ("http-code", "rpc-code")) == "code-unknown"
    assert "" in edge_tokens(dict(code, dst_label="File"))


def _gold():
    sites = [
        _site("http", CALL, "GET /users/me", "c/cli", "cli.py", "Cli.me"),
        _site("http", CALL, "POST /users", "c/cli", "other.py", "Cli.add"),
        _site("http", HANDLER, "GET /users/me", "s/srv", "me.go", "Server.Me", ("Me",)),
        _site("http", HANDLER, "POST /users", "s/srv", "users.go", "Server.Add", ("Add",)),
        _site("http", SPEC, "GET /users/me", "s/srv", "api.yaml", "GET /users/me", ("GET /users/me",)),
    ]
    return build_pairs(sites)["pairs"]


def test_score_group_precision_recall():
    names = {"id-c": "c/cli", "id-s": "s/srv"}
    edges = [
        _edge("id-c", "cli.py", "Cli.me", "id-s", "me.go", "server.Server.Me", "Method"),  # correct http-code
        _edge("id-c", "cli.py", "Cli.me", "id-s", "users.go", "server.Server.Add", "Method"),  # wrong target
        _edge("id-c", "cli.py", "Cli.me", "id-s", "api.yaml", "api.yaml:paths./users/me.get", "Route"),  # correct http-spec
        _edge("id-c", "tests/test_cli.py", "t", "id-s", "me.go", "server.Server.Me", "Method"),  # out of scope
        _edge("id-c", "cli.py", "Cli.me", "id-s", "me.go", "server.Server.Me", "Method", strategy="import"),  # not a contract edge
        _edge("id-c", "cli.py", "Cli.me", "id-s", "x.proto", "p.S.M", "RpcMethod"),  # a kind the group does not score
    ]
    sc = score_group(edges, _gold(), names, ("http-code", "http-spec"))
    assert sc["by_kind"]["http-code"] == {"predicted": 2, "correct": 1, "gold": 2, "found": 1}
    assert sc["by_kind"]["http-spec"] == {"predicted": 1, "correct": 1, "gold": 1, "found": 1}
    assert sc["out_of_scope"] == 1 and sc["unscored"] == {"rpc-spec-client": 1}
    assert sc["missed_count"] == 1 and sc["wrong_count"] == 1
    groups = [{"name": "g", "split": "gated", "status": "ok", "score": sc}, {"name": "d", "split": "diagnostic", "status": "ok", "score": sc}]
    summary = pooled(groups)
    assert summary["http-code"]["precision"] == 0.5 and summary["http-code"]["recall"] == 0.5
    assert summary["all"]["predicted"] == 3 and summary["all"]["correct"] == 2 and summary["all"]["gold"] == 3
    assert summary["all"]["precision_ci"] is not None, "bootstrap over source files"
    assert pooled(groups) == summary, "deterministic"


def test_scorecard_block_gates_per_kind():
    def judge(spec, value, ci, n, min_n):
        return "pending" if value is None else ("✅" if value >= spec["floor"] else "❌")

    s2 = {"summary": {"http-code": {"precision": 0.95, "recall": 0.8, "predicted": 20, "gold": 25}, "all": {"precision": 0.5, "recall": 0.5}}}
    card = scorecard_block(s2, judge)
    assert card["metrics"]["http-code_precision"]["status"] == "✅" and card["metrics"]["http-code_recall"]["status"] == "✅"
    assert card["metrics"]["all_precision"]["gated"] is False
    assert "http-spec_precision" in card["failing"] and "all_precision" not in card["failing"]
    assert card["status"] == "preview"


def test_groups_are_pinned_and_audit_sample_is_fixed():
    assert all(len(r["commit"]) == 40 for g in GROUPS for r in g["repos"])
    assert all(len(g["repos"]) >= 2 for g in GROUPS), "the gold is multi-repository"
    pairs = [p.as_dict() for p in _gold()]
    assert audit_sample(pairs, 1) == audit_sample(list(reversed(pairs)), 1)


def test_score_record_from_a_raw_run():
    gold = {"pairs": [p.as_dict() for p in _gold()], "counts": {"http-code": 2, "http-spec": 1}, "dangling": [{"key": "GET /x"}]}
    edges = [_edge("id-c", "cli.py", "Cli.me", "id-s", "me.go", "server.Server.Me", "Method")]
    raw = {
        "status": "ok",
        "arm_version": "rueca test",
        "groups": [
            {"name": "g", "split": "gated", "kinds": ["http-code", "http-spec"], "repos": [], "status": "ok", "repo_ids": {"c/cli": "id-c", "s/srv": "id-s"}, "edges": edges, "dangling": [{"key": "GET /x"}], "provided": [["s/srv", "me.go", "Me", "http"]], "gold": gold},
            {"name": "h", "split": "gated", "kinds": ["rpc-code"], "repos": [], "status": "error", "error": "boom"},
        ],
    }
    s2 = score_record(raw)
    g = s2["groups"][0]
    assert g["contract_edges"] == 1 and g["dangling"] == 1 and g["gold_dangling"] == 1
    assert s2["summary"]["http-code"]["correct"] == 1 and s2["summary"]["http-code"]["gold"] == 2
    assert s2["groups"][1]["status"] == "error" and not s2["complete"]
    assert _entries({"contracts": [{"key": "a"}], "truncated": False}) == [{"key": "a"}]
    assert _entries({"other": [1]}) == [1] and _entries(None) is None


def test_high_confidence_summary_and_publishing():
    gold = {"pairs": [p.as_dict() for p in _gold()], "counts": {"http-code": 2, "http-spec": 1}, "dangling": []}
    edges = [
        _edge("id-c", "cli.py", "Cli.me", "id-s", "me.go", "server.Server.Me", "Method"),
        dict(_edge("id-c", "cli.py", "Cli.me", "id-s", "users.go", "server.Server.Add", "Method"), confidence=0.5),
    ]
    raw = {"status": "ok", "groups": [{"name": "g", "split": "gated", "kinds": ["http-code"], "repos": [], "status": "ok", "repo_ids": {"c/cli": "id-c", "s/srv": "id-s"}, "edges": edges, "dangling": [], "provided": [], "gold": gold}]}
    s2 = score_record(raw)
    assert s2["summary"]["http-code"]["precision"] == 0.5 and s2["summary_high"]["http-code"]["precision"] == 1.0
    assert s2["high_confidence"] == 0.7

    def judge(spec, value, ci, n, min_n):
        return "pending" if value is None else ("✅" if value >= spec["floor"] else "❌")

    block = scorecard_block(s2, judge)
    card = with_contracts({"generated": "d", "languages": {}, "config": {}, "settings": {}, "rueca": "v"}, block)
    assert list(card) == ["generated", "languages", "config", "settings", "contracts", "rueca"]
    md = "\n".join(["## Scorecard", "", "| configuration | `stable` | — |", "", "### Cross-repository edges (packages)", "", "x", "", "### Multi-ref (one repository at two commits)", "", "y", ""])
    once = splice_s2(md, s2, block)
    assert "| cross-service edges (S2) |" in once and "### Cross-service edges (S2, contracts)" in once
    assert splice_s2(once, s2, block) == once, "idempotent: the S2 rows are replaced, nothing else moves"
    assert once.replace(splice_s2(md, s2, block), "") == "" and once.split("### Multi-ref")[1] == md.split("### Multi-ref")[1]
