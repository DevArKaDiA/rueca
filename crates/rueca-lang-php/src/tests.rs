//! Extraction tests. Name-resolution rules live in `names.rs`, Composer /
//! import resolution in `composer.rs`, variable scopes in `locals.rs`.

use super::*;
use rueca_model::{Definition, Label, RefKind, Reference};

fn facts(src: &str) -> FileFacts {
    PhpPack::default().extract("x.php", Language::Php, src.as_bytes(), &Budget::default()).unwrap()
}

fn def<'a>(f: &'a FileFacts, qn: &str) -> &'a Definition {
    f.definitions.iter().find(|d| d.qualified_name == qn).unwrap_or_else(|| {
        let all: Vec<&str> = f.definitions.iter().map(|d| d.qualified_name.as_str()).collect();
        panic!("no definition {qn}; have {all:#?}")
    })
}

/// References as `(kind, receiver + name, enclosing qualified name)`;
/// receiver and name are joined with a space so the encoding stays visible.
fn refs(f: &FileFacts) -> Vec<(RefKind, String, String)> {
    f.references.iter().map(|r| (r.kind, ref_text(r), enc_name(f, r))).collect()
}

fn ref_text(r: &Reference) -> String {
    match &r.receiver {
        Some(recv) => format!("{recv} {}", r.name),
        None => r.name.clone(),
    }
}

fn enc_name(f: &FileFacts, r: &Reference) -> String {
    r.enclosing.map(|e| f.definitions[e as usize].qualified_name.clone()).unwrap_or_default()
}

#[track_caller]
fn assert_refs(f: &FileFacts, expected: &[(RefKind, &str, &str)]) {
    let got = refs(f);
    for (k, t, e) in expected {
        assert!(
            got.iter().any(|(gk, gt, ge)| gk == k && gt == t && ge == e),
            "missing {k:?} `{t}` in `{e}`; got {got:#?}"
        );
    }
}

fn names_of_kind(f: &FileFacts, kind: RefKind) -> Vec<String> {
    f.references.iter().filter(|r| r.kind == kind).map(ref_text).collect()
}

fn summary(f: &FileFacts) -> Vec<(Label, &str, Option<&str>)> {
    f.definitions
        .iter()
        .map(|d| {
            (d.label, d.qualified_name.as_str(), d.parent.map(|p| f.definitions[p as usize].qualified_name.as_str()))
        })
        .collect()
}

macro_rules! fixture {
    ($path:literal) => {
        include_str!(concat!("../tests/fixtures/laravel/", $path))
    };
}

const CONTROLLER: &str = fixture!("app/Http/Controllers/UserController.php");
const MODEL: &str = fixture!("app/Models/User.php");
const SERVICE: &str = fixture!("app/Services/UserService.php");
const ENUM: &str = fixture!("app/Enums/Status.php");
const INTERFACE: &str = fixture!("app/Contracts/Notifier.php");
const TRAIT: &str = fixture!("app/Models/Concerns/HasUuid.php");
const HELPERS: &str = fixture!("app/Support/helpers.php");
const PHPUNIT: &str = fixture!("tests/Unit/UserServiceTest.php");
const PEST: &str = fixture!("tests/Feature/UserPestTest.php");
const VIEW: &str = fixture!("resources/views/users.php");
const ROUTES: &str = fixture!("routes/web.php");
const SYMFONY: &str = fixture!("src/Controller/BlogController.php");
const POST: &str = fixture!("src/Entity/Post.php");
const LEGACY: &str = fixture!("src/legacy.php");

// ---------------------------------------------------------------------------
// Definitions
// ---------------------------------------------------------------------------

#[test]
fn controller_definitions() {
    let f = facts(CONTROLLER);
    use Label::*;
    let c = Some("App\\Http\\Controllers\\UserController");
    assert_eq!(
        summary(&f),
        vec![
            (Class, "App\\Http\\Controllers\\UserController", None),
            (Constant, "App\\Http\\Controllers\\UserController::PER_PAGE", c),
            (Method, "App\\Http\\Controllers\\UserController::__construct", c),
            (Field, "App\\Http\\Controllers\\UserController::$users", c),
            (Method, "App\\Http\\Controllers\\UserController::index", c),
            (Method, "App\\Http\\Controllers\\UserController::store", c),
            (Method, "App\\Http\\Controllers\\UserController::show", c),
        ]
    );
    assert_eq!(f.parse_errors, 0);
    assert_eq!(f.line_count, 55);
}

#[test]
fn controller_definition_details() {
    let f = facts(CONTROLLER);
    let class = def(&f, "App\\Http\\Controllers\\UserController");
    assert_eq!(class.name, "UserController");
    assert_eq!(class.signature.as_deref(), Some("class UserController extends Controller"));
    assert_eq!(class.doc.as_deref(), Some("CRUD for users."));
    assert_eq!(class.bases, vec!["\\App\\Http\\Controllers\\Controller"]);
    assert!(class.exported && !class.is_test);
    assert_eq!(&CONTROLLER[class.name_span.start_byte as usize..class.name_span.end_byte as usize], "UserController");

    let index = def(&f, "App\\Http\\Controllers\\UserController::index");
    assert_eq!(index.signature.as_deref(), Some("public function index(Request $request): JsonResponse"));
    assert_eq!(index.doc.as_deref(), Some("List active users."));
    assert!(index.exported);

    let users = def(&f, "App\\Http\\Controllers\\UserController::$users");
    assert_eq!(users.name, "$users");
    assert_eq!(users.label, Label::Field);
    assert_eq!(users.signature.as_deref(), Some("private readonly UserService $users"));
    assert!(!users.exported);

    let per_page = def(&f, "App\\Http\\Controllers\\UserController::PER_PAGE");
    assert_eq!(per_page.signature.as_deref(), Some("public const PER_PAGE = 20"));
    assert!(per_page.exported);
}

#[test]
fn controller_references() {
    let f = facts(CONTROLLER);
    use RefKind::*;
    let index = "App\\Http\\Controllers\\UserController::index";
    let store = "App\\Http\\Controllers\\UserController::store";
    let show = "App\\Http\\Controllers\\UserController::show";
    assert_refs(
        &f,
        &[
            // Promoted property type is attributed to the field.
            (Type, "\\App\\Services\\ UserService", "App\\Http\\Controllers\\UserController::$users"),
            (Type, "\\Illuminate\\Http\\ Request", index),
            (Type, "\\Illuminate\\Http\\ JsonResponse", index),
            (Call, "$this authorizeAccess", index),
            // Typed parameter: `$request` is a Request.
            (Call, "\\Illuminate\\Http\\Request-> query", index),
            // Facade from a grouped `use`.
            (Call, "\\Illuminate\\Support\\Facades\\Cache remember", index),
            // `$this->users` is a promoted `UserService` property.
            (Read, "$this $users", index),
            (Call, "\\App\\Services\\UserService-> active", index),
            (Read, "self PER_PAGE", index),
            // Unqualified function in a namespace: namespace, then global.
            (Call, "App\\Http\\Controllers\\ response", index),
            (Call, "response() json", index),
            (Call, "\\App\\Services\\UserService-> register", store),
            (Call, "\\App\\Http\\Requests\\StoreUserRequest-> validated", store),
            (Call, "\\Illuminate\\Support\\Facades\\Log info", store),
            (New, "\\Illuminate\\Http\\ JsonResponse", store),
            // `use function`.
            (Call, "\\App\\Support\\ format_money", store),
            (Call, "$user balance", store),
            (Type, "\\App\\Models\\ User", show),
            (Call, "\\App\\Models\\User findOrFail", show),
            (Read, "\\App\\Enums\\Status Banned", show),
            (Throw, "\\ DomainException", show),
            (New, "\\ DomainException", show),
        ],
    );
    // Variables are never references.
    for r in &f.references {
        assert!(!r.name.starts_with('$') || r.receiver.as_deref() == Some("$this"), "{r:?}");
    }
    assert!(!names_of_kind(&f, RefKind::Type).iter().any(|t| t.ends_with(" int")));
}

#[test]
fn controller_imports() {
    let f = facts(CONTROLLER);
    type ImportRow = (String, Vec<(String, Option<String>)>, Option<String>);
    let got: Vec<ImportRow> = f
        .imports
        .iter()
        .map(|i| {
            (
                i.module.clone(),
                i.names.iter().map(|n| (n.name.clone(), n.alias.clone())).collect(),
                i.module_alias.clone(),
            )
        })
        .collect();
    let class = |m: &str, a: &str| (m.to_string(), vec![], Some(a.to_string()));
    assert_eq!(
        got,
        vec![
            class("App\\Enums\\Status", "Status"),
            class("App\\Http\\Requests\\StoreUserRequest", "StoreUserRequest"),
            class("App\\Models\\User", "User"),
            class("App\\Services\\UserService", "UserService"),
            class("Illuminate\\Http\\JsonResponse", "JsonResponse"),
            class("Illuminate\\Http\\Request", "Request"),
            class("Illuminate\\Support\\Facades\\Cache", "Cache"),
            class("Illuminate\\Support\\Facades\\Log", "Log"),
            ("App\\Support".to_string(), vec![("format_money".to_string(), None)], None),
        ]
    );
    assert!(f.imports.iter().all(|i| i.relative_level == 0 && !i.type_only && !i.reexport));
}

#[test]
fn model_traits_constants_and_relative_receivers() {
    let f = facts(MODEL);
    let user = def(&f, "App\\Models\\User");
    // Parent class first, then used traits.
    assert_eq!(
        user.bases,
        vec![
            "\\Illuminate\\Database\\Eloquent\\Model",
            "\\Illuminate\\Database\\Eloquent\\Factories\\HasFactory",
            "\\App\\Models\\Concerns\\HasUuid",
        ]
    );
    assert_eq!(user.doc.as_deref(), Some("An account holder.\n\n@property int $id"));
    assert_eq!(
        def(&f, "App\\Models\\User::$fillable").signature.as_deref(),
        Some("protected $fillable = ['name', 'email']")
    );
    assert!(def(&f, "App\\Models\\User::active").signature.as_deref().unwrap().starts_with("public static function"));
    use RefKind::*;
    assert_refs(
        &f,
        &[
            // `Status::class` in a property default reads the class.
            (Read, "\\App\\Enums\\ Status", "App\\Models\\User::$casts"),
            (Read, "\\App\\Models\\ Order", "App\\Models\\User::orders"),
            (Call, "$this hasMany", "App\\Models\\User::orders"),
            (Read, "$this $refunds", "App\\Models\\User::balance"),
            (Call, "static query", "App\\Models\\User::active"),
            (Read, "\\App\\Enums\\Status Active", "App\\Models\\User::active"),
            (Write, "$this $status", "App\\Models\\User::promote"),
            (Call, "$this ensureUuid", "App\\Models\\User::promote"),
        ],
    );
    // Traits are bases, not type references.
    assert!(!names_of_kind(&f, Type).iter().any(|t| t.contains("HasUuid")));
}

#[test]
fn service_receiver_typing_from_promoted_properties() {
    let f = facts(SERVICE);
    let register = "App\\Services\\UserService::register";
    use RefKind::*;
    assert_refs(
        &f,
        &[
            (Read, "$this $repository", register),
            (Call, "\\App\\Repositories\\UserRepository-> create", register),
            (Call, "\\App\\Contracts\\Notifier-> notify", register),
            // Imported global class.
            (Throw, "\\ InvalidArgumentException", register),
            // `$user` comes from an untyped call: unknown receiver.
            (Call, "$user promote", register),
        ],
    );
    let sig = def(&f, "App\\Services\\UserService::__construct").signature.clone().unwrap();
    assert_eq!(sig, "public function __construct(private UserRepository $repository, private Notifier $notifier)");
    assert_eq!(def(&f, "App\\Services\\UserService").signature.as_deref(), Some("final class UserService"));
}

#[test]
fn enum_interface_and_trait() {
    let f = facts(ENUM);
    use Label::*;
    let e = Some("App\\Enums\\Status");
    assert_eq!(
        summary(&f),
        vec![
            (Enum, "App\\Enums\\Status", None),
            (Constant, "App\\Enums\\Status::Active", e),
            (Constant, "App\\Enums\\Status::Banned", e),
            (Method, "App\\Enums\\Status::label", e),
            (Method, "App\\Enums\\Status::fromLabel", e),
        ]
    );
    assert_eq!(def(&f, "App\\Enums\\Status").signature.as_deref(), Some("enum Status: string"));
    assert_eq!(def(&f, "App\\Enums\\Status::Active").signature.as_deref(), Some("case Active = 'active'"));
    assert!(def(&f, "App\\Enums\\Status::Active").exported);
    assert_refs(
        &f,
        &[
            (RefKind::Read, "$this $value", "App\\Enums\\Status::label"),
            (RefKind::Call, "self from", "App\\Enums\\Status::fromLabel"),
            (RefKind::Call, "App\\Enums\\ strtolower", "App\\Enums\\Status::fromLabel"),
        ],
    );

    let f = facts(INTERFACE);
    assert_eq!(
        summary(&f),
        vec![
            (Interface, "App\\Contracts\\Notifier", None),
            (Constant, "App\\Contracts\\Notifier::CHANNEL", Some("App\\Contracts\\Notifier")),
            (Method, "App\\Contracts\\Notifier::notify", Some("App\\Contracts\\Notifier")),
        ]
    );
    assert_eq!(
        def(&f, "App\\Contracts\\Notifier::notify").signature.as_deref(),
        Some("public function notify(User $user, string $message): bool")
    );

    let f = facts(TRAIT);
    assert_eq!(summary(&f)[0], (Trait, "App\\Models\\Concerns\\HasUuid", None));
    assert_refs(&f, &[(RefKind::Write, "$this $uuid", "App\\Models\\Concerns\\HasUuid::ensureUuid")]);
    assert_refs(
        &f,
        &[(RefKind::Call, "\\Illuminate\\Support\\Str uuid", "App\\Models\\Concerns\\HasUuid::ensureUuid")],
    );
}

#[test]
fn namespaced_functions_and_constants() {
    let f = facts(HELPERS);
    use Label::*;
    assert_eq!(
        summary(&f),
        vec![(Constant, "App\\Support\\CURRENCY", None), (Function, "App\\Support\\format_money", None)]
    );
    let fm = def(&f, "App\\Support\\format_money");
    assert_eq!(fm.doc.as_deref(), Some("Formats cents as money."));
    assert!(fm.exported);
    assert_refs(
        &f,
        &[
            (RefKind::Call, "App\\Support\\ function_exists", ""),
            (RefKind::Call, "App\\Support\\ number_format", "App\\Support\\format_money"),
            (RefKind::Read, "App\\Support\\ CURRENCY", "App\\Support\\format_money"),
        ],
    );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[test]
fn phpunit_tests() {
    let f = facts(PHPUNIT);
    let class = def(&f, "Tests\\Unit\\UserServiceTest");
    assert!(class.is_test);
    assert_eq!(class.bases, vec!["\\Tests\\TestCase"]);
    assert!(def(&f, "Tests\\Unit\\UserServiceTest::testRegisterRequiresEmail").is_test);
    assert!(def(&f, "Tests\\Unit\\UserServiceTest::it_lists_active_users").is_test);
    assert!(def(&f, "Tests\\Unit\\UserServiceTest::registers").is_test);
    assert!(!def(&f, "Tests\\Unit\\UserServiceTest::setUp").is_test);
    assert_eq!(def(&f, "Tests\\Unit\\UserServiceTest::registers").decorators, vec!["Test"]);
    let set_up = "Tests\\Unit\\UserServiceTest::setUp";
    assert_refs(
        &f,
        &[
            (RefKind::Call, "parent setUp", set_up),
            (RefKind::Write, "$this $service", set_up),
            (RefKind::New, "\\App\\Services\\ UserService", set_up),
            (RefKind::Call, "$this createMock", set_up),
            (RefKind::Read, "\\App\\Contracts\\ Notifier", set_up),
            (RefKind::Decorator, "\\PHPUnit\\Framework\\Attributes\\ Test", "Tests\\Unit\\UserServiceTest::registers"),
            // Typed property `$service`.
            (RefKind::Call, "\\App\\Services\\UserService-> register", "Tests\\Unit\\UserServiceTest::registers"),
        ],
    );
    // A class extending `*TestCase` is a test even without the suffix.
    let f = facts(
        "<?php\nclass Checks extends \\PHPUnit\\Framework\\TestCase { public function testA() {} public function a() {} }\n",
    );
    assert!(def(&f, "Checks").is_test && def(&f, "Checks::testA").is_test && !def(&f, "Checks::a").is_test);
    // `test*` methods outside test classes are not tests.
    let f = facts("<?php\nclass Service { public function testConnection() {} }\n");
    assert!(!def(&f, "Service::testConnection").is_test);
}

#[test]
fn pest_tests() {
    let f = facts(PEST);
    use Label::*;
    assert_eq!(
        summary(&f),
        vec![
            (Test, "users", None),
            (Test, "users > promotes a user", Some("users")),
            (Test, "users > balance starts at zero", Some("users")),
            (Test, "lists users", None),
        ]
    );
    let t = def(&f, "users > promotes a user");
    assert!(t.is_test && !t.exported);
    assert_eq!(t.name, "promotes a user");
    assert_eq!(t.signature.as_deref(), Some("it('promotes a user', function ()"));
    assert_eq!(
        def(&f, "users > balance starts at zero").signature.as_deref(),
        Some("test('balance starts at zero', fn () =>")
    );
    assert_refs(
        &f,
        &[
            (RefKind::Call, "\\ beforeEach", "users"),
            (RefKind::Call, "\\App\\Models\\User factory", "users"),
            (RefKind::Call, "$this->user promote", "users > promotes a user"),
            (RefKind::Call, "\\App\\Models\\User find", "users > balance starts at zero"),
            (RefKind::Call, "$this get", "lists users"),
        ],
    );
    // Recognized test functions are definitions, not calls.
    assert!(!names_of_kind(&f, RefKind::Call).iter().any(|c| c == "\\ it" || c == "\\ describe" || c == "\\ test"));
    // Without a closure `it()` is just a call.
    let f = facts("<?php\nit('todo');\n");
    assert!(f.definitions.is_empty());
    assert_eq!(names_of_kind(&f, RefKind::Call), vec!["\\ it"]);
}

// ---------------------------------------------------------------------------
// Templates, routes, attributes, namespaces
// ---------------------------------------------------------------------------

#[test]
fn html_template() {
    let f = facts(VIEW);
    assert_eq!(f.parse_errors, 0);
    assert!(f.definitions.is_empty());
    assert_refs(
        &f,
        &[
            (RefKind::Call, "\\ htmlspecialchars", ""),
            (RefKind::Call, "\\App\\Models\\User active", ""),
            (RefKind::Call, "$user->status label", ""),
            (RefKind::Call, "\\ e", ""),
        ],
    );
    assert_eq!(f.imports.len(), 1);
    assert_eq!(f.imports[0].module, "__DIR__/partials/footer.php");
    // A file with no PHP at all.
    let f = facts("<html><body>{{ $blade }}</body></html>\n");
    assert_eq!(f, FileFacts { line_count: 1, ..FileFacts::default() });
}

#[test]
fn routes_and_facades() {
    let f = facts(ROUTES);
    assert_refs(
        &f,
        &[
            (RefKind::Call, "\\Illuminate\\Support\\Facades\\Route get", ""),
            (RefKind::Read, "\\App\\Http\\Controllers\\ UserController", ""),
            (RefKind::Call, "\\Illuminate\\Support\\Facades\\Route post", ""),
        ],
    );
    let modules: Vec<&str> = f.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(
        modules,
        [
            "App\\Http\\Controllers\\UserController",
            "Illuminate\\Support\\Facades\\Route",
            "__DIR__/../app/Support/helpers.php"
        ]
    );
}

#[test]
fn symfony_attributes() {
    let f = facts(SYMFONY);
    let class = def(&f, "Acme\\Blog\\Controller\\BlogController");
    assert_eq!(class.decorators, vec!["Route('/blog', name: 'blog_')"]);
    let show = def(&f, "Acme\\Blog\\Controller\\BlogController::show");
    assert_eq!(show.decorators, vec!["Route('/{slug}', name: 'show', methods: ['GET'])", "IsGranted('ROLE_USER')"]);
    assert_eq!(show.signature.as_deref(), Some("public function show(Post $post): Response"));
    // Attributes are inside the span; the name span is the identifier.
    assert!(show.span.contains(&show.name_span));
    assert_refs(
        &f,
        &[
            (
                RefKind::Decorator,
                "\\Symfony\\Component\\Routing\\Attribute\\ Route",
                "Acme\\Blog\\Controller\\BlogController",
            ),
            (
                RefKind::Decorator,
                "\\Symfony\\Component\\Routing\\Attribute\\ Route",
                "Acme\\Blog\\Controller\\BlogController::show",
            ),
            (
                RefKind::Decorator,
                "\\Symfony\\Component\\Security\\Http\\Attribute\\ IsGranted",
                "Acme\\Blog\\Controller\\BlogController::show",
            ),
            (RefKind::Call, "$this render", "Acme\\Blog\\Controller\\BlogController::show"),
            (RefKind::Call, "\\Acme\\Blog\\Entity\\Post-> publish", "Acme\\Blog\\Controller\\BlogController::show"),
        ],
    );

    let f = facts(POST);
    let title = def(&f, "Acme\\Blog\\Entity\\Post::$title");
    assert_eq!(title.decorators, vec!["\\SensitiveParameter"]);
    assert!(title.exported);
    assert!(!def(&f, "Acme\\Blog\\Entity\\Post::$publishedAt").exported);
    // Parameter attributes are not part of the constructor signature.
    assert_eq!(
        def(&f, "Acme\\Blog\\Entity\\Post::__construct").signature.as_deref(),
        Some(
            "public function __construct(public string $title = '', protected ?\\DateTimeImmutable $publishedAt = null)"
        )
    );
    assert_refs(&f, &[(RefKind::Decorator, "\\ SensitiveParameter", "Acme\\Blog\\Entity\\Post::$title")]);
}

#[test]
fn block_namespaces_and_grouped_use() {
    let f = facts(LEGACY);
    assert_eq!(
        summary(&f),
        vec![(Label::Function, "Acme\\Legacy\\make", None), (Label::Function, "legacy_boot", None)]
    );
    assert_refs(
        &f,
        &[
            (RefKind::Type, "\\Acme\\Blog\\Entity\\ Post", "Acme\\Legacy\\make"),
            (RefKind::New, "\\Acme\\Blog\\Entity\\ Post", "Acme\\Legacy\\make"),
            // `use Acme\Legacy;` in the second (global) block: `Legacy\make()`.
            (RefKind::Call, "\\Acme\\Legacy\\ make", "legacy_boot"),
        ],
    );
    let modules: Vec<(&str, Option<&str>)> =
        f.imports.iter().map(|i| (i.module.as_str(), i.module_alias.as_deref())).collect();
    assert_eq!(
        modules,
        [
            ("Acme\\Blog\\Entity\\Post", Some("Post")),
            ("Acme\\Blog\\Controller\\BlogController", Some("Blog")),
            ("Acme\\Legacy", Some("Legacy")),
        ]
    );
}

#[test]
fn name_resolution_forms() {
    let src = r#"<?php
namespace App\Http;
use Vendor\Lib as L;
use function Vendor\fn_a, Vendor\fn_b as fb;
use const Vendor\LIMIT;
use Vendor\{Util\Str, function helper, const SIZE};
function f() {
    new L\Client();
    new namespace\Local();
    new \Root();
    new Sub\Thing();
    fn_a(); fb(); helper(); \strlen('x'); strlen('y'); Sub\g(); namespace\h();
    return [LIMIT, SIZE, OTHER, \PHP_EOL, PHP_EOL, Str::slug('a')];
}
"#;
    let f = facts(src);
    let got = names_of_kind(&f, RefKind::New);
    assert_eq!(got, ["\\Vendor\\Lib\\ Client", "\\App\\Http\\ Local", "\\ Root", "\\App\\Http\\Sub\\ Thing"]);
    assert_eq!(
        names_of_kind(&f, RefKind::Call),
        [
            "\\Vendor\\ fn_a",
            "\\Vendor\\ fn_b",
            "\\Vendor\\ helper",
            "\\ strlen",
            "App\\Http\\ strlen",
            "\\App\\Http\\Sub\\ g",
            "\\App\\Http\\ h",
            "\\Vendor\\Util\\Str slug",
        ]
    );
    // Builtin constants without a namespace are skipped; `\PHP_EOL` is kept.
    assert_eq!(
        names_of_kind(&f, RefKind::Read),
        ["\\Vendor\\ LIMIT", "\\Vendor\\ SIZE", "App\\Http\\ OTHER", "\\ PHP_EOL"]
    );
    let imports: Vec<(String, Vec<String>, Option<String>)> = f
        .imports
        .iter()
        .map(|i| {
            (
                i.module.clone(),
                i.names
                    .iter()
                    .map(|n| {
                        format!("{}{}", n.name, n.alias.as_deref().map(|a| format!(" as {a}")).unwrap_or_default())
                    })
                    .collect(),
                i.module_alias.clone(),
            )
        })
        .collect();
    assert_eq!(
        imports,
        vec![
            ("Vendor\\Lib".into(), vec![], Some("L".into())),
            ("Vendor".into(), vec!["fn_a".into()], None),
            ("Vendor".into(), vec!["fn_b as fb".into()], None),
            ("Vendor".into(), vec!["LIMIT".into()], None),
            ("Vendor\\Util\\Str".into(), vec![], Some("Str".into())),
            ("Vendor".into(), vec!["helper".into()], None),
            ("Vendor".into(), vec!["SIZE".into()], None),
        ]
    );
}

#[test]
fn include_paths() {
    let src = r#"<?php
require __DIR__ . '/../vendor/autoload.php';
require_once dirname(__DIR__) . '/config.php';
include_once dirname(__DIR__, 2) . DIRECTORY_SEPARATOR . 'x.php';
include "lib/a.php";
require dirname(__FILE__) . '/b.php';
require $path;
require base_path('c.php');
"#;
    let f = facts(src);
    let modules: Vec<&str> = f.imports.iter().map(|i| i.module.as_str()).collect();
    assert_eq!(
        modules,
        [
            "__DIR__/../vendor/autoload.php",
            "__DIR__/../config.php",
            "__DIR__/../../x.php",
            "lib/a.php",
            "__DIR__/b.php"
        ]
    );
    // A dynamic path is code.
    assert_eq!(names_of_kind(&f, RefKind::Call), ["\\ base_path"]);
}

// ---------------------------------------------------------------------------
// References
// ---------------------------------------------------------------------------

#[test]
fn receiver_typing() {
    let src = r#"<?php
namespace App;
use App\Models\User;
class Svc {
    private ?Mailer $mailer;
    public function a(User $u, ?Repo $r, A|B $ab, self $me) {
        $u->save();
        $r?->find();
        $ab->m();
        $me->other();
        $n = new Builder();
        $n->build();
        $fn = fn () => $u->arrow();
        $cl = function () use ($u) { $u->captured(); };
        $nc = function () { $u->notCaptured(); };
        $this->mailer->send();
        try { } catch (Failed $e) { $e->report(); }
        $u = $u->fresh();
    }
    public function b(User $u) { $u->kept(); }
    /** @var \App\Log\Logger */
    protected $logger;
    /**
     * @param  \App\Cache\Store  $store
     * @param  string  $key
     */
    public function c($store, $key) { $store->get($key); $key->x(); $this->logger->info(); }
}
"#;
    let f = facts(src);
    let calls = names_of_kind(&f, RefKind::Call);
    for expected in [
        // `$u` in `a` is reassigned to an untyped value: unknown.
        "$u save",
        "\\App\\Repo-> find",
        "$ab m",
        "self other",
        "\\App\\Builder-> build",
        "$u arrow",
        "$u captured",
        "$u notCaptured",
        "\\App\\Mailer-> send",
        "\\App\\Failed-> report",
        "\\App\\Models\\User-> kept",
        // Docblock types: `@param` for untyped parameters, `@var` for
        // untyped properties.
        "\\App\\Cache\\Store-> get",
        "$key x",
        "\\App\\Log\\Logger-> info",
    ] {
        assert!(calls.contains(&expected.to_string()), "missing {expected}: {calls:#?}");
    }
}

#[test]
fn types_instanceof_catch_and_unions() {
    let src = r#"<?php
namespace N;
function f(?A $a, B|C|null $b, D&E $d, (F&G)|null $dnf, int $i, mixed $m, static|self $s): never {
    if ($a instanceof H) {}
    if ($a instanceof self) {}
    try {} catch (I | \J $e) {}
}
"#;
    let f = facts(src);
    let types = names_of_kind(&f, RefKind::Type);
    assert_eq!(
        types,
        ["\\N\\ A", "\\N\\ B", "\\N\\ C", "\\N\\ D", "\\N\\ E", "\\N\\ F", "\\N\\ G", "\\N\\ H", "\\N\\ I", "\\ J"]
    );
}

#[test]
fn anonymous_classes_and_closures() {
    let src = r#"<?php
namespace N;
class Outer {
    public function make() {
        $x = new class($dep) extends Base implements Iface {
            use T;
            public function run(P $p): R { return $this->inner() + self::K; }
        };
        $f = static function (Q $q) use ($x): S { return helper($q); };
        return $x;
    }
}
"#;
    let f = facts(src);
    // Members of the anonymous class are not definitions.
    assert_eq!(
        summary(&f),
        vec![(Label::Class, "N\\Outer", None), (Label::Method, "N\\Outer::make", Some("N\\Outer"))]
    );
    let make = "N\\Outer::make";
    assert_refs(
        &f,
        &[
            (RefKind::Type, "\\N\\ Base", make),
            (RefKind::Type, "\\N\\ Iface", make),
            (RefKind::Type, "\\N\\ T", make),
            (RefKind::Type, "\\N\\ P", make),
            (RefKind::Type, "\\N\\ R", make),
            (RefKind::Call, "class@anonymous inner", make),
            (RefKind::Read, "class@anonymous K", make),
            (RefKind::Type, "\\N\\ Q", make),
            (RefKind::Type, "\\N\\ S", make),
            (RefKind::Call, "N\\ helper", make),
        ],
    );
}

#[test]
fn reads_writes_and_dynamic_access() {
    let src = r#"<?php
class C {
    public static $count;
    public function m($name) {
        $this->a = 1;
        $this->a = 2;
        $this->b += 1;
        $this->c++;
        self::$count = $this->d;
        static::$count++;
        $x = $this->d;
        $this->cache['k'] = $this->e;
        $this->$name();
        $this->{$name} = 1;
        $fn = 'strlen';
        $fn('x');
        $obj->prop = 3;
        echo "{$this->f} and $this->g";
    }
}
"#;
    let f = facts(src);
    let writes = names_of_kind(&f, RefKind::Write);
    assert_eq!(writes, ["$this $a", "$this $b", "$this $c", "self $count", "static $count"]);
    let reads = names_of_kind(&f, RefKind::Read);
    assert_eq!(reads, ["$this $d", "$this $cache", "$this $e", "$this $f", "$this $g"]);
    assert!(names_of_kind(&f, RefKind::Call).is_empty(), "{:?}", names_of_kind(&f, RefKind::Call));
}

#[test]
fn new_self_static_parent_and_static_access() {
    let src = r#"<?php
class C extends P {
    public static function make() {
        new self(); new static; new parent();
        parent::__construct();
        $k = static::class;
        return self::$instance ?? Other::$cache ?? Other::LIMIT;
    }
}
"#;
    let f = facts(src);
    assert_eq!(names_of_kind(&f, RefKind::New), ["self __construct", "static __construct", "parent __construct"]);
    assert_eq!(names_of_kind(&f, RefKind::Call), ["parent __construct"]);
    assert_eq!(names_of_kind(&f, RefKind::Read), ["self $instance", "\\Other $cache", "\\Other LIMIT"]);
}

#[test]
fn define_and_conditional_declarations() {
    let src = r#"<?php
/** App version. */
define('APP_VERSION', '1.0');
define('Vendor\NS_CONST', 1);
define($dynamic, 2);
if (!class_exists('Polyfill')) {
    class Polyfill {}
}
function outer() {
    function inner() {}
    class Local {}
}
"#;
    let f = facts(src);
    use Label::*;
    assert_eq!(
        summary(&f),
        vec![
            (Constant, "APP_VERSION", None),
            (Constant, "Vendor\\NS_CONST", None),
            (Class, "Polyfill", None),
            (Function, "outer", None),
            (Function, "inner", Some("outer")),
            (Class, "Local", Some("outer")),
        ]
    );
    let c = def(&f, "APP_VERSION");
    assert_eq!(c.signature.as_deref(), Some("define('APP_VERSION', '1.0')"));
    assert_eq!(c.doc.as_deref(), Some("App version."));
    assert_eq!(def(&f, "Vendor\\NS_CONST").name, "NS_CONST");
}

#[test]
fn property_hooks_and_interface_members() {
    let src = r#"<?php
class Person {
    public string $fullName {
        get => $this->first . ' ' . $this->last;
        set(string $value) { $this->split($value); }
    }
    public function __construct(public private(set) string $first = '') {}
}
"#;
    let f = facts(src);
    assert!(f.definitions.iter().any(|d| d.qualified_name == "Person::$fullName"));
    assert_refs(
        &f,
        &[(RefKind::Read, "$this $first", "Person::$fullName"), (RefKind::Call, "$this split", "Person::$fullName")],
    );
}

#[test]
fn docblock_placement() {
    let src = r#"<?php
class C {
    /** Before attributes. */
    #[A]
    public function a() {}

    #[B]
    /** After attributes. */
    public function b() {}

    /** Not adjacent. */

    // separated by a line comment
    public function c() {}

    /* Plain comment. */
    public function d() {}
}
"#;
    let f = facts(src);
    assert_eq!(def(&f, "C::a").doc.as_deref(), Some("Before attributes."));
    assert_eq!(def(&f, "C::b").doc.as_deref(), Some("After attributes."));
    assert_eq!(def(&f, "C::c").doc, None);
    assert_eq!(def(&f, "C::d").doc, None);
    assert_eq!(def(&f, "C::b").signature.as_deref(), Some("public function b()"));
}

// ---------------------------------------------------------------------------
// Robustness and the pack contract
// ---------------------------------------------------------------------------

#[test]
fn parse_errors_are_counted_and_partial_facts_kept() {
    let f = facts("<?php\nfunction ok() {}\nfunction broken( {\nclass After {}\n");
    assert!(f.parse_errors > 0);
    assert!(f.definitions.iter().any(|d| d.name == "ok"));
}

#[test]
fn deterministic() {
    for src in [CONTROLLER, MODEL, PEST, VIEW, LEGACY] {
        assert_eq!(facts(src), facts(src));
    }
}

#[test]
fn reference_cap() {
    let mut src = String::from("<?php\nfunction f() {\n");
    for i in 0..25_000 {
        src.push_str(&format!("c{i}();\n"));
    }
    src.push_str("}\n");
    let budget = Budget { max_bytes: 10 * 1024 * 1024, ..Budget::default() };
    let f = PhpPack::default().extract("m.php", Language::Php, src.as_bytes(), &budget).unwrap();
    assert_eq!(f.references.len(), MAX_REFS);
}

#[test]
fn depth_budget_prevents_deep_recursion() {
    let n = 5000;
    let src = format!("<?php\nfunction f() {{ return {}1{}; }}\n", "(".repeat(n), ")".repeat(n));
    let budget = Budget { max_depth: 64, ..Budget::default() };
    let f = PhpPack::default().extract("m.php", Language::Php, src.as_bytes(), &budget).unwrap();
    assert_eq!(f.definitions.len(), 1);
    let src = format!("<?php\n$x = {}a(){};\n", "[".repeat(n), "]".repeat(n));
    let f = PhpPack::default().extract("m.php", Language::Php, src.as_bytes(), &Budget::default()).unwrap();
    assert!(f.definitions.is_empty());
}

#[test]
fn errors_for_budget_and_language() {
    let budget = Budget { max_bytes: 4, ..Budget::default() };
    assert!(matches!(
        PhpPack::default().extract("m.php", Language::Php, b"<?php echo 1;\n", &budget),
        Err(ExtractError::TooLarge { .. })
    ));
    assert!(matches!(
        PhpPack::default().extract("m.py", Language::Python, b"", &Budget::default()),
        Err(ExtractError::Unsupported(Language::Python))
    ));
    assert_eq!(facts(""), FileFacts::default());
}

/// Small deterministic xorshift PRNG (no external dependency).
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn random_bytes_never_panic() {
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let tokens: &[&[u8]] = &[
        b"<?php ",
        b"?>",
        b"namespace ",
        b"use ",
        b"function ",
        b"class ",
        b"interface ",
        b"trait ",
        b"enum ",
        b"case ",
        b"extends ",
        b"implements ",
        b"new ",
        b"(",
        b")",
        b"{",
        b"}",
        b"[",
        b"]",
        b";",
        b",",
        b"\\",
        b"::",
        b"->",
        b"?->",
        b"$this",
        b"$x",
        b"self",
        b"parent",
        b"static ",
        b"#[",
        b"/**",
        b"*/",
        b"'",
        b"\"",
        b"=",
        b"=>",
        b"fn",
        b"public ",
        b"const ",
        b"Foo",
        b"bar",
        b"require ",
        b"__DIR__",
        b".",
        b"it(",
        b"define(",
        b"\n",
        b"\xff\xfe",
        b"\xc3",
        b"<html>",
    ];
    for round in 0..400 {
        let len = (rng.next() % 600) as usize;
        let mut src = Vec::with_capacity(len * 4);
        if round % 4 == 1 {
            src.extend_from_slice(b"<?php ");
        }
        for _ in 0..len {
            if round % 2 == 0 {
                src.push(rng.next() as u8);
            } else {
                src.extend_from_slice(tokens[(rng.next() as usize) % tokens.len()]);
            }
        }
        let pack = PhpPack::default();
        let a = pack.extract("r.php", Language::Php, &src, &Budget::default()).unwrap();
        let b = pack.extract("r.php", Language::Php, &src, &Budget::default()).unwrap();
        assert_eq!(a, b);
        for d in &a.definitions {
            assert!(d.span.contains(&d.name_span) || d.parent.is_some(), "{d:?}");
            if let Some(p) = d.parent {
                assert!((p as usize) < a.definitions.len());
            }
        }
        for r in &a.references {
            if let Some(e) = r.enclosing {
                assert!((e as usize) < a.definitions.len());
            }
        }
    }
}

#[test]
fn builtins_are_sorted_and_cover_common_names() {
    assert!(builtins::BUILTINS.windows(2).all(|w| w[0] < w[1]), "BUILTINS must be sorted and unique");
    let p = PhpPack::default();
    for b in [
        "strlen",
        "array_map",
        "count",
        "json_encode",
        "sprintf",
        "preg_match",
        "Exception",
        "RuntimeException",
        "DateTime",
        "Closure",
        "stdClass",
        "ArrayAccess",
        "Iterator",
        "Countable",
        "PHP_EOL",
        "__DIR__",
        "StrLen",
        "\\strlen",
    ] {
        assert!(p.is_builtin(b), "{b}");
    }
    for n in ["User", "format_money", "app", "collect", "$this"] {
        assert!(!p.is_builtin(n), "{n}");
    }
}

#[test]
fn pack_contract() {
    let p = PhpPack::default();
    assert_eq!(p.languages(), &[Language::Php]);
    assert_eq!(p.self_receivers(), &["$this", "self", "static"]);
    let project = ProjectFiles {
        paths: ["app/Models/User.php", "app/Http/Kernel.php"].iter().map(|s| s.to_string()).collect(),
        configs: [("composer.json".to_string(), r#"{"autoload":{"psr-4":{"App\\":"app/"}}}"#.to_string())]
            .into_iter()
            .collect(),
    };
    assert_eq!(p.module_name("app/Models/User.php", &project), "");
    assert_eq!(expected_namespace("app/Models/User.php", &project).as_deref(), Some("App\\Models"));
    let f = facts(CONTROLLER);
    let user = f.imports.iter().find(|i| i.module == "App\\Models\\User").unwrap();
    assert_eq!(p.resolve_import("app/Http/Kernel.php", user, &project).as_deref(), Some("app/Models/User.php"));
    let request = f.imports.iter().find(|i| i.module == "Illuminate\\Http\\Request").unwrap();
    assert_eq!(p.resolve_import("app/Http/Kernel.php", request, &project), None);
}
