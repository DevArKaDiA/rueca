//! Fixture tests of the Go contract hints: one test per framework or rule,
//! negatives included.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{ContractKind, ContractSide, FileFacts, Language};

use crate::GoPack;

fn facts(src: &str) -> FileFacts {
    GoPack::default().extract("x.go", Language::Go, src.as_bytes(), &Budget::default()).unwrap()
}

/// Hints as `(side, kind, key, owner qualified name, framework)`.
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

fn provided(src: &str) -> Vec<(String, String)> {
    keys(src, ContractSide::Provides, ContractKind::Http)
}

fn consumed(src: &str) -> Vec<(String, String)> {
    keys(src, ContractSide::Consumes, ContractKind::Http)
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn gin_routes_groups_and_handlers() {
    let src = r#"package server
import (
    "net/http"
    "github.com/gin-gonic/gin"
)
type Server struct{}
func (s *Server) GenerateHandler(c *gin.Context) {}
func (s *Server) ListHandler(c *gin.Context) {}
func (s *Server) withLogging(p string, h gin.HandlerFunc) []gin.HandlerFunc { return nil }
const blobs = "/api/blobs"
func (s *Server) Routes() http.Handler {
    r := gin.Default()
    r.GET("/", func(c *gin.Context) {})
    r.POST("/api/generate", s.withLogging("/api/generate", s.GenerateHandler)...)
    r.Any("/api/any", s.ListHandler)
    r.Handle("DELETE", "/api/delete", s.ListHandler)
    r.HEAD(blobs+"/:digest", s.ListHandler)
    v1 := r.Group("/v1")
    v1.GET("/models/:model",
        middleware(),
        s.ListHandler,
    )
    r.GET("/api/other", s.OtherFileHandler)
    r.GET(dynamicPath, s.ListHandler)
    return r
}
"#;
    assert_eq!(
        provided(src),
        pairs(&[
            ("GET /", "Server.Routes"),
            ("POST /api/generate", "Server.GenerateHandler"),
            ("* /api/any", "Server.ListHandler"),
            ("DELETE /api/delete", "Server.ListHandler"),
            ("HEAD /api/blobs/{}", "Server.ListHandler"),
            ("GET /v1/models/{}", "Server.ListHandler"),
            ("GET /api/other", "Server.Routes"),
        ])
    );
    let f = facts(src);
    let other = f.contracts.iter().find(|h| h.key == "GET /api/other").unwrap();
    assert!(other.raw.contains("s.OtherFileHandler"), "{}", other.raw);
    let multi = f.contracts.iter().find(|h| h.key == "GET /v1/models/{}").unwrap();
    assert_eq!(multi.raw, "v1.GET(\"/models/:model\", middleware(), s.ListHandler, )");
    assert!(f.contracts.iter().all(|h| h.framework == "gin"));
}

#[test]
fn routes_need_the_router_import() {
    let src = r#"package x
func Routes(r Router) {
    r.GET("/a", h)
    r.Get("/b", h)
}
func h() {}
"#;
    assert_eq!(provided(src), vec![]);
}

#[test]
fn net_http_mux_and_method_patterns() {
    let src = r#"package x
import "net/http"
func health(w http.ResponseWriter, r *http.Request) {}
func Serve() {
    http.HandleFunc("/healthz", health)
    mux := http.NewServeMux()
    mux.Handle("GET /items/{id}", http.HandlerFunc(health))
    mux.HandleFunc("POST example.com/items", health)
    mux.HandleFunc("not a path", health)
}
"#;
    assert_eq!(provided(src), pairs(&[("* /healthz", "health"), ("GET /items/{}", "health")]));
    assert!(hints(src).iter().all(|h| h.4 == "net/http"));
}

#[test]
fn echo_fiber_and_match() {
    let echo = r#"package x
import "github.com/labstack/echo/v4"
func list(c echo.Context) error { return nil }
func Routes(e *echo.Echo) {
    g := e.Group("/admin")
    g.GET("/users", list)
    e.Match([]string{"GET", "POST"}, "/both", list)
    e.Add("PUT", "/put", list)
}
"#;
    assert_eq!(
        provided(echo),
        pairs(&[("GET /admin/users", "list"), ("GET /both", "list"), ("POST /both", "list"), ("PUT /put", "list")])
    );
    assert!(hints(echo).iter().all(|h| h.4 == "echo"));
    let fiber = r#"package x
import "github.com/gofiber/fiber/v2"
func get(c *fiber.Ctx) error { return nil }
func Routes(app *fiber.App) {
    api := app.Group("/api")
    api.Get("/users/:id", get)
    app.All("/any", get)
}
"#;
    assert_eq!(provided(fiber), pairs(&[("GET /api/users/{}", "get"), ("* /any", "get")]));
    assert!(hints(fiber).iter().all(|h| h.4 == "fiber"));
}

#[test]
fn chi_route_closures_and_mount() {
    let src = r#"package x
import (
    "net/http"
    "github.com/go-chi/chi/v5"
)
func getUser(w http.ResponseWriter, r *http.Request) {}
func Routes(r chi.Router) {
    r.Get("/ping", getUser)
    r.Route("/users", func(r chi.Router) {
        r.Get("/{id}", getUser)
        r.Route("/{id}/posts", func(sub chi.Router) {
            sub.Post("/", getUser)
        })
    })
    r.Group(func(g chi.Router) {
        g.Delete("/x", getUser)
    })
    r.Method("PATCH", "/m", http.HandlerFunc(getUser))
    r.Mount("/api", apiRouter())
    r.Get("/after", getUser)
}
"#;
    assert_eq!(
        provided(src),
        pairs(&[
            ("GET /ping", "getUser"),
            ("GET /users/{}", "getUser"),
            ("POST /users/{}/posts", "getUser"),
            ("DELETE /x", "getUser"),
            ("PATCH /m", "getUser"),
            ("GET /after", "getUser"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "chi"));
}

#[test]
fn gorilla_methods_and_subrouters() {
    let src = r#"package x
import (
    "net/http"
    "github.com/gorilla/mux"
)
type API struct{}
func (a *API) Items(w http.ResponseWriter, r *http.Request) {}
func (a *API) Routes() {
    r := mux.NewRouter()
    r.HandleFunc("/items", a.Items).Methods("GET", http.MethodPost)
    s := r.PathPrefix("/v2").Subrouter()
    s.HandleFunc("/items/{id}", a.Items).Methods(http.MethodDelete)
    r.Handle("/raw", nil)
}
"#;
    assert_eq!(
        provided(src),
        pairs(&[
            ("GET /items", "API.Items"),
            ("POST /items", "API.Items"),
            ("DELETE /v2/items/{}", "API.Items"),
            ("* /raw", "API.Routes"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "gorilla"));
}

#[test]
fn net_http_consumers() {
    let src = r#"package x
import (
    "context"
    "fmt"
    "net/http"
)
func Calls(ctx context.Context, base, id, m string) {
    http.Get("https://api.example.com/v1/users?x=1")
    http.Post(base+"/api/items", "application/json", nil)
    http.NewRequest("PUT", fmt.Sprintf("%s/api/blobs/%s", base, id), nil)
    http.NewRequestWithContext(ctx, http.MethodDelete, base+"/api/items/"+id, nil)
    http.NewRequest(m, base+"/api/dyn", nil)
    http.NewRequest("GET", base, nil)
    client := &http.Client{}
    client.Get("http://localhost:8080/status")
    http.DefaultClient.Head("/head")
    http.NewRequest(m, "http://"+host+path, nil)
}
"#;
    assert_eq!(
        consumed(src),
        pairs(&[
            ("GET /v1/users", "Calls"),
            ("POST /api/items", "Calls"),
            ("PUT /api/blobs/{}", "Calls"),
            ("DELETE /api/items/{}", "Calls"),
            ("* /api/dyn", "Calls"),
            ("GET /status", "Calls"),
            ("HEAD /head", "Calls"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "net/http"));
}

#[test]
fn sdk_wrappers_with_method_and_path() {
    let src = r#"package api
import (
    "context"
    "fmt"
    "net/http"
)
type Client struct{}
func (c *Client) do(ctx context.Context, method, path string, req, resp any) error {
    request, _ := http.NewRequestWithContext(ctx, method, c.base.String()+path, nil)
    _ = request
    return nil
}
func (c *Client) stream(ctx context.Context, method, path string, data any) error { return nil }
func (c *Client) Generate(ctx context.Context) error {
    return c.stream(ctx, http.MethodPost, "/api/generate", nil)
}
func (c *Client) List(ctx context.Context) error {
    return c.do(ctx, "GET", "/api/tags", nil, nil)
}
func (c *Client) Blob(ctx context.Context, d string) error {
    return c.do(ctx, http.MethodHead, fmt.Sprintf("/api/blobs/%s", d), nil, nil)
}
func (c *Client) NotHTTP(ctx context.Context) error {
    return c.do(ctx, "fetch", "/api/x", nil, nil)
}
"#;
    assert_eq!(
        consumed(src),
        pairs(&[
            ("POST /api/generate", "Client.Generate"),
            ("GET /api/tags", "Client.List"),
            ("HEAD /api/blobs/{}", "Client.Blob"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "http-client"));
}

#[test]
fn resty_and_httptest() {
    let src = r#"package x
import (
    "net/http/httptest"
    "github.com/go-resty/resty/v2"
)
func Calls(c *resty.Client) {
    c.R().SetBody(1).Post("/api/orders")
    c.R().Execute("PATCH", "/api/orders/1")
    httptest.NewRequest("GET", "/api/test", nil)
}
"#;
    assert_eq!(consumed(src), pairs(&[("POST /api/orders", "Calls"), ("PATCH /api/orders/1", "Calls")]));
    assert!(hints(src).iter().all(|h| h.4 == "resty"));
}

fn rpc(src: &str, side: ContractSide) -> Vec<(String, String)> {
    keys(src, side, ContractKind::Rpc)
}

#[test]
fn grpc_servers_by_embedding_and_registration() {
    let src = r#"package x
import (
    "context"
    "google.golang.org/grpc"
    pb "example.com/helloworld/proto"
)
type server struct {
    pb.UnimplementedGreeterServer
}
func (s *server) SayHello(ctx context.Context, in *pb.HelloRequest) (*pb.HelloReply, error) { return nil, nil }
func (s *server) RouteChat(stream pb.Greeter_RouteChatServer) error { return nil }
func (s *server) Close() error { return nil }
func (s *server) helper(ctx context.Context) {}
func (s *server) Run(ctx context.Context) error { return nil }
type other struct{}
func (o *other) Ping(ctx context.Context, in *pb.PingRequest) (*pb.Pong, error) { return nil, nil }
func Serve(s *grpc.Server) {
    pb.RegisterHealthServer(s, &other{})
}
"#;
    assert_eq!(
        rpc(src, ContractSide::Provides),
        pairs(&[
            ("Greeter/SayHello", "server.SayHello"),
            ("Greeter/RouteChat", "server.RouteChat"),
            ("Health/Ping", "other.Ping"),
            // Service markers on the server types (expanded by the resolver
            // over the methods of every file of the package).
            ("Greeter/*", "server"),
            ("Health/*", "other"),
        ])
    );
    assert!(hints(src).iter().all(|h| h.4 == "grpc-go"));
}

#[test]
fn grpc_wrapper_servers_embed_the_server_interface() {
    let src = r#"package x
import (
    "context"
    "github.com/dapr/durabletask-go/api/protos"
)
type fallback struct {
    protos.TaskHubSidecarServiceServer
}
type stream struct {
    protos.TaskHubSidecarService_GetWorkItemsServer
}
type plain struct {
    other.ThingServer
}
func (s fallback) RaiseEvent(ctx context.Context, req *protos.RaiseEventRequest) (*protos.RaiseEventResponse, error) { return nil, nil }
"#;
    // The embedded server interface makes `fallback` a server (its
    // methods here and, through the marker, in other files); a stream type
    // and a non-proto interface do not.
    assert_eq!(
        rpc(src, ContractSide::Provides),
        pairs(&[("TaskHubSidecarService/RaiseEvent", "fallback.RaiseEvent"), ("TaskHubSidecarService/*", "fallback")])
    );
}

#[test]
fn grpc_registration_through_variables_and_receivers() {
    let src = r#"package x
import (
    "google.golang.org/grpc"
    pb "example.com/helloworld/proto"
)
type Server struct{}
type Scheduler struct{}
func newService(gs *grpc.Server) *Server {
    s := &Server{}
    pb.RegisterAppCallbackAlphaServer(gs, s)
    return s
}
func (sch *Scheduler) Run(gs *grpc.Server) {
    pb.RegisterSchedulerServer(gs, sch)
}
func register(gs *grpc.Server, unknown any) {
    pb.RegisterGreeterServer(gs, unknown)
}
"#;
    assert_eq!(
        rpc(src, ContractSide::Provides),
        pairs(&[("AppCallbackAlpha/*", "Server"), ("Scheduler/*", "Scheduler")])
    );
}

#[test]
fn grpc_server_methods_by_shape() {
    let src = r#"package grpc
import (
    "context"
    "google.golang.org/grpc"
    "google.golang.org/protobuf/types/known/emptypb"
    runtimev1pb "github.com/dapr/dapr/pkg/proto/runtime/v1"
)
func (a *api) InvokeService(ctx context.Context, in *runtimev1pb.InvokeServiceRequest) (*runtimev1pb.InvokeResponse, error) { return nil, nil }
func (a *api) GetConfigurationAlpha1(ctx context.Context, in *runtimev1pb.GetConfigurationRequest) (*runtimev1pb.GetConfigurationResponse, error) { return nil, nil }
func (a *api) SaveState(ctx context.Context, in *runtimev1pb.SaveStateRequest) (*emptypb.Empty, error) { return nil, nil }
func (a *api) SubscribeTopicEventsAlpha1(stream runtimev1pb.Dapr_SubscribeTopicEventsAlpha1Server) error { return nil }
func (a *api) Other(ctx context.Context, in *runtimev1pb.SaveStateRequest) (*runtimev1pb.X, error) { return nil, nil }
func (a *api) DeleteBinaryFileAlpha1(ctx context.Context, req *runtimev1pb.DeleteBinaryFileRequestAlpha1) (*runtimev1pb.DeleteBinaryFileResponseAlpha1, error) { return nil, nil }
func (a *api) GetAlpha1(ctx context.Context, req *runtimev1pb.GetRequestBeta1) (*runtimev1pb.X, error) { return nil, nil }
func (a *api) Mismatch(ctx context.Context, in *runtimev1pb.MismatchRequest) (int, error) { return 0, nil }
func (c *client) InvokeService(ctx context.Context, in *runtimev1pb.InvokeServiceRequest, opts ...grpc.CallOption) (*runtimev1pb.InvokeResponse, error) { return nil, nil }
"#;
    assert_eq!(
        rpc(src, ContractSide::Provides),
        pairs(&[
            ("*/InvokeService", "api.InvokeService"),
            ("*/GetConfigurationAlpha1", "api.GetConfigurationAlpha1"),
            ("*/SaveState", "api.SaveState"),
            ("Dapr/SubscribeTopicEventsAlpha1", "api.SubscribeTopicEventsAlpha1"),
            ("*/DeleteBinaryFileAlpha1", "api.DeleteBinaryFileAlpha1"),
        ])
    );
    // The shape does not need a gRPC import (a shared implementation
    // package such as dapr's `universal` imports only the proto package);
    // without a proto import nothing is a handler.
    let no_grpc = src.replace("\"google.golang.org/grpc\"\n", "");
    assert_eq!(rpc(&no_grpc, ContractSide::Provides).len(), 5);
    let no_proto = src.replace("runtimev1pb \"github.com/dapr/dapr/pkg/proto/runtime/v1\"\n", "");
    assert!(rpc(&no_proto, ContractSide::Provides).is_empty());
}

#[test]
fn grpc_clients() {
    let src = r#"package client
import (
    "context"
    "google.golang.org/grpc"
    "github.com/dapr/dapr/pkg/client"
    pb "github.com/dapr/dapr/pkg/proto/runtime/v1"
)
type GRPCClient struct {
    protoClient pb.DaprClient
}
func (c *GRPCClient) Shutdown(ctx context.Context) error {
    _, err := c.protoClient.Shutdown(ctx, &pb.ShutdownRequest{})
    return err
}
func Hello(conn *grpc.ClientConn, ctx context.Context) {
    client := pb.NewGreeterClient(conn)
    client.SayHello(ctx, &pb.HelloRequest{})
    pb.NewGreeterClient(conn).SayBye(ctx, nil)
    other.Thing(ctx)
}
func (c *GRPCClient) Elsewhere(ctx context.Context) {
    c.grpcClient.InvokeBinding(ctx, nil)
    c.grpcClient.Close()
    c.grpcClient.helper(ctx)
    c.kubeClient.List(ctx, nil)
    c.kubeClient.GetState(ctx, &pb.GetStateRequest{})
    client.GetOperatorClient(ctx)
}
func Stream(s pb.Dapr_ChatClient) { s.Recv() }
type DaprGRPCClient struct{}
func (c *DaprGRPCClient) Wrap(ctx context.Context) error { return c.Other(ctx, nil) }
"#;
    assert_eq!(
        rpc(src, ContractSide::Consumes),
        pairs(&[
            ("Dapr/Shutdown", "GRPCClient.Shutdown"),
            ("Greeter/SayHello", "Hello"),
            ("Greeter/SayBye", "Hello"),
            ("*/InvokeBinding", "GRPCClient.Elsewhere"),
            ("*/GetState", "GRPCClient.Elsewhere"),
        ])
    );
    // No proto import: a `client` receiver is not enough.
    let plain = r#"package x
import "context"
func F(ctx context.Context) { c.httpClient.Get(ctx, "k") }
"#;
    assert_eq!(rpc(plain, ContractSide::Consumes), vec![]);
}

#[test]
fn generated_files_have_no_hints() {
    let src = r#"// Code generated by protoc-gen-go-grpc. DO NOT EDIT.

package pb
import "net/http"
func F() { http.Get("/x") }
"#;
    assert_eq!(hints(src), vec![]);
}
