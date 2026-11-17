//! Fixture tests of the Java contract hints: one test per framework or
//! rule, negatives included.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{ContractKind, ContractSide, FileFacts, Language};

use crate::JavaPack;

fn facts(src: &str) -> FileFacts {
    JavaPack::default().extract("src/X.java", Language::Java, src.as_bytes(), &Budget::default()).unwrap()
}

/// `(key, owner qualified name, framework)` of the hints of one side and kind.
fn keys(src: &str, side: ContractSide, kind: ContractKind) -> Vec<(String, String, String)> {
    let f = facts(src);
    f.contracts
        .iter()
        .filter(|h| h.side == side && h.kind == kind)
        .map(|h| {
            let owner = h.owner.map(|o| f.definitions[o as usize].qualified_name.clone()).unwrap_or_default();
            (h.key.clone(), owner, h.framework.clone())
        })
        .collect()
}

fn triples(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
}

fn provided(src: &str) -> Vec<(String, String, String)> {
    keys(src, ContractSide::Provides, ContractKind::Http)
}

fn consumed(src: &str) -> Vec<(String, String, String)> {
    keys(src, ContractSide::Consumes, ContractKind::Http)
}

#[test]
fn spring_mappings_with_class_prefix() {
    let src = r#"
package demo;
import org.springframework.web.bind.annotation.*;
@RestController
@RequestMapping("/api")
public class OwnerController {
    private static final String BY_ID = "/owners/{ownerId}";
    @GetMapping("/owners")
    public List<Owner> list() { return null; }
    @GetMapping(BY_ID)
    public Owner show(@PathVariable int ownerId) { return null; }
    @PostMapping(value = {"/owners", "/owners/new"})
    public void create() {}
    @RequestMapping(value = "/owners/{id}", method = {RequestMethod.PUT, RequestMethod.PATCH})
    public void update() {}
    @RequestMapping(path = "/any")
    public void any() {}
    @DeleteMapping
    public void deleteAll() {}
    public void helper() {}
}
"#;
    assert_eq!(
        provided(src),
        triples(&[
            ("GET /api/owners", "OwnerController.list", "spring"),
            ("GET /api/owners/{}", "OwnerController.show", "spring"),
            ("POST /api/owners", "OwnerController.create", "spring"),
            ("POST /api/owners/new", "OwnerController.create", "spring"),
            ("PUT /api/owners/{}", "OwnerController.update", "spring"),
            ("PATCH /api/owners/{}", "OwnerController.update", "spring"),
            ("* /api/any", "OwnerController.any", "spring"),
            ("DELETE /api", "OwnerController.deleteAll", "spring"),
        ])
    );
}

#[test]
fn mappings_need_the_framework_and_skip_plain_interfaces() {
    let no_import = r#"
class C { @GetMapping("/x") void x() {} }
"#;
    assert_eq!(provided(no_import), vec![]);
    let iface = r#"
import org.springframework.web.bind.annotation.GetMapping;
interface Api { @GetMapping("/x") String x(); }
"#;
    assert_eq!(provided(iface), vec![]);
    assert_eq!(consumed(iface), vec![]);
}

#[test]
fn jaxrs_and_micronaut() {
    let jaxrs = r#"
import jakarta.ws.rs.*;
@Path("/users")
public class UserResource {
    @GET
    public List<User> list() { return null; }
    @GET
    @Path("/{id}")
    public User get(@PathParam("id") String id) { return null; }
    @POST @PUT
    @Path("{id}/tags")
    public void tag() {}
    @Path("/sub")
    public Sub locator() { return null; }
}
"#;
    assert_eq!(
        provided(jaxrs),
        triples(&[
            ("GET /users", "UserResource.list", "jaxrs"),
            ("GET /users/{}", "UserResource.get", "jaxrs"),
            ("POST /users/{}/tags", "UserResource.tag", "jaxrs"),
            ("PUT /users/{}/tags", "UserResource.tag", "jaxrs"),
        ])
    );
    let micronaut = r#"
import io.micronaut.http.annotation.*;
@Controller("/pets")
public class PetController {
    @Get("/{name}")
    public Pet byName(String name) { return null; }
    @Post
    public void save() {}
}
"#;
    assert_eq!(
        provided(micronaut),
        triples(&[
            ("GET /pets/{}", "PetController.byName", "micronaut"),
            ("POST /pets", "PetController.save", "micronaut")
        ])
    );
}

#[test]
fn client_interfaces_are_consumers() {
    let feign = r#"
import org.springframework.cloud.openfeign.FeignClient;
import org.springframework.web.bind.annotation.*;
import feign.RequestLine;
@FeignClient(name = "stores", path = "/v1")
public interface StoreClient {
    @GetMapping("/stores/{storeId}")
    Store get(@PathVariable Long storeId);
    @RequestLine("POST /stores")
    void create(Store s);
}
"#;
    assert_eq!(
        consumed(feign),
        triples(&[
            ("GET /v1/stores/{}", "StoreClient.get", "feign"),
            ("POST /v1/stores", "StoreClient.create", "feign")
        ])
    );
    assert_eq!(provided(feign), vec![]);
    let exchange = r#"
import org.springframework.web.service.annotation.*;
@HttpExchange("/repos")
public interface RepoService {
    @GetExchange("/{owner}/{repo}")
    Repo get(String owner, String repo);
}
"#;
    assert_eq!(consumed(exchange), triples(&[("GET /repos/{}/{}", "RepoService.get", "spring")]));
}

#[test]
fn http_client_calls() {
    let src = r#"
import org.springframework.web.client.RestTemplate;
import org.springframework.web.reactive.function.client.WebClient;
import org.springframework.http.HttpMethod;
import java.net.http.HttpRequest;
import java.net.URI;
import okhttp3.Request;
public class Calls {
    private static final String BASE = "http://users-service:8080";
    void run(RestTemplate restTemplate, WebClient webClient, String base, long id) {
        restTemplate.getForObject(BASE + "/users/{id}", User.class, id);
        restTemplate.exchange(base + "/users/" + id, HttpMethod.DELETE, null, Void.class);
        this.restTemplate.put("/users/1", body);
        map.put("/not/http", 1);
        webClient.post().uri("/orders/{id}/items", id).retrieve().bodyToMono(String.class);
        webClient.method(HttpMethod.PATCH).uri(String.format("%s/orders/%d", base, id)).retrieve();
        HttpRequest.newBuilder(URI.create("https://api.example.com/v2/items?x=1")).POST(body).build();
        HttpRequest.newBuilder().uri(URI.create(base + "/health")).build();
        new Request.Builder().url(base + "/okhttp/x").delete().build();
    }
}
"#;
    let src = src.replace("import okhttp3.Request;", "import okhttp3.Request; // okhttp3");
    assert_eq!(
        consumed(&src),
        triples(&[
            ("GET /users/{}", "Calls.run", "resttemplate"),
            ("DELETE /users/{}", "Calls.run", "resttemplate"),
            ("PUT /users/1", "Calls.run", "resttemplate"),
            ("POST /orders/{}/items", "Calls.run", "webclient"),
            ("PATCH /orders/{}", "Calls.run", "webclient"),
            ("POST /v2/items", "Calls.run", "java.net.http"),
            ("GET /health", "Calls.run", "java.net.http"),
            ("DELETE /okhttp/x", "Calls.run", "okhttp"),
        ])
    );
}

fn rpc(src: &str, side: ContractSide) -> Vec<(String, String, String)> {
    keys(src, side, ContractKind::Rpc)
}

#[test]
fn grpc_servers_and_stubs() {
    let server = r#"
import io.grpc.stub.StreamObserver;
public class GreeterImpl extends GreeterGrpc.GreeterImplBase {
    @Override
    public void sayHello(HelloRequest req, StreamObserver<HelloReply> responseObserver) {}
    @Override
    public StreamObserver<Note> routeChat(StreamObserver<Note> out) { return null; }
    public void helper(String s) {}
    private void hidden(StreamObserver<X> o) {}
}
class Async implements HealthGrpc.AsyncService {
    public void check(HealthCheckRequest r, StreamObserver<HealthCheckResponse> o) {}
}
"#;
    assert_eq!(
        rpc(server, ContractSide::Provides),
        triples(&[
            ("Greeter/SayHello", "GreeterImpl.sayHello", "grpc-java"),
            ("Greeter/RouteChat", "GreeterImpl.routeChat", "grpc-java"),
            ("Health/Check", "Async.check", "grpc-java"),
        ])
    );
    let client = r#"
import io.grpc.ManagedChannel;
public class Client {
    private final GreeterGrpc.GreeterBlockingStub blockingStub;
    Client(ManagedChannel channel) {
        blockingStub = GreeterGrpc.newBlockingStub(channel);
    }
    void greet(ManagedChannel channel) {
        blockingStub.sayHello(req);
        this.blockingStub.withDeadlineAfter(1, SECONDS).sayBye(req);
        var async = RouteGuideGrpc.newStub(channel);
        async.routeChat(observer);
        HealthGrpc.newFutureStub(channel).check(req);
        other.sayHello(req);
    }
}
"#;
    assert_eq!(
        rpc(client, ContractSide::Consumes),
        triples(&[
            ("Greeter/SayHello", "Client.greet", "grpc-java"),
            ("Greeter/SayBye", "Client.greet", "grpc-java"),
            ("RouteGuide/RouteChat", "Client.greet", "grpc-java"),
            ("Health/Check", "Client.greet", "grpc-java"),
        ])
    );
    // Without io.grpc in the file, nothing.
    assert_eq!(rpc(&client.replace("io.grpc", "x.y"), ContractSide::Consumes), vec![]);
}
