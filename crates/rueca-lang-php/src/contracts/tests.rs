//! Contract hint tests: inline sources, every pattern of the module docs
//! and the negatives next to them.

use rueca_extract::{Budget, LanguagePack};
use rueca_model::{ContractKind, ContractSide, FileFacts, Language};

use crate::PhpPack;

fn facts(src: &str) -> FileFacts {
    PhpPack::default().extract("x.php", Language::Php, src.as_bytes(), &Budget::default()).unwrap()
}

/// `(side, key, owner qualified name, framework)` of the HTTP hints.
fn hints(src: &str) -> Vec<(ContractSide, String, String, String)> {
    let f = facts(src);
    f.contracts
        .iter()
        .inspect(|h| assert_eq!(h.kind, ContractKind::Http))
        .map(|h| {
            let owner = h.owner.map(|o| f.definitions[o as usize].qualified_name.clone()).unwrap_or_default();
            (h.side, h.key.clone(), owner, h.framework.clone())
        })
        .collect()
}

fn keys(src: &str, side: ContractSide) -> Vec<(String, String, String)> {
    hints(src).into_iter().filter(|h| h.0 == side).map(|h| (h.1, h.2, h.3)).collect()
}

fn triples(v: &[(&str, &str, &str)]) -> Vec<(String, String, String)> {
    v.iter().map(|(a, b, c)| (a.to_string(), b.to_string(), c.to_string())).collect()
}

use ContractSide::{Consumes, Provides};

#[test]
fn laravel_routes_and_groups() {
    let src = r#"<?php
use Illuminate\Support\Facades\Route;
use App\Http\Controllers\UserController;

Route::get('/', fn () => view('welcome'));
Route::post('users/{id}', [UserController::class, 'update']);
Route::match(['get', 'post'], '/form', 'FormController@handle');
Route::any('/hook', 'HookController@handle');
Route::middleware('auth')->delete('/session', [SessionController::class, 'destroy']);
Route::prefix('api/')->middleware('api')->group(function () {
    Route::get('/users/{user}', [UserController::class, 'show']);
    Route::group(['prefix' => 'v2'], function () {
        Route::patch('items/{item?}', 'ItemController@patch');
    });
});
Route::apiResource('photos', PhotoController::class);
Route::get($dynamic, 'X@y');
"#;
    let got = keys(src, Provides);
    let want = triples(&[
        ("GET /", "", "laravel"),
        ("POST /users/{}", "", "laravel"),
        ("GET /form", "", "laravel"),
        ("POST /form", "", "laravel"),
        ("* /hook", "", "laravel"),
        ("DELETE /session", "", "laravel"),
        ("GET /api/users/{}", "", "laravel"),
        ("PATCH /api/v2/items/{}", "", "laravel"),
        ("GET /photos", "", "laravel"),
        ("POST /photos", "", "laravel"),
        ("GET /photos/{}", "", "laravel"),
        ("PUT /photos/{}", "", "laravel"),
        ("PATCH /photos/{}", "", "laravel"),
        ("DELETE /photos/{}", "", "laravel"),
    ]);
    assert_eq!(got, want);
    assert!(keys(src, Consumes).is_empty());
}

#[test]
fn symfony_attributes() {
    let src = r#"<?php
namespace App\Controller;

use Symfony\Component\Routing\Attribute\Route;

#[Route('/blog', name: 'blog_')]
class BlogController
{
    #[Route('/{slug}', name: 'show', methods: ['GET', 'HEAD'])]
    public function show(string $slug) {}

    #[Route(path: '/new')]
    public function create() {}

    public function helper() {}
}
"#;
    assert_eq!(
        keys(src, Provides),
        triples(&[
            ("GET /blog/{}", "App\\Controller\\BlogController::show", "symfony"),
            ("HEAD /blog/{}", "App\\Controller\\BlogController::show", "symfony"),
            ("* /blog/new", "App\\Controller\\BlogController::create", "symfony"),
        ])
    );
}

#[test]
fn slim_routes() {
    let src = r#"<?php
$app->get('/hello/{name}', function ($request, $response, $args) {});
$router->post('/users', 'UserController@store');
$app->get('/not-a-route', ['timeout' => 2]);
"#;
    assert_eq!(keys(src, Provides), triples(&[("GET /hello/{}", "", "php-router"), ("POST /users", "", "php-router")]));
    assert_eq!(keys(src, Consumes), triples(&[("GET /not-a-route", "", "php-http")]));
}

#[test]
fn http_clients() {
    let src = r#"<?php
namespace App\Services;

use GuzzleHttp\Client;
use Illuminate\Support\Facades\Http;
use Illuminate\Support\Facades\Storage;

class Api
{
    public function run($id, $method)
    {
        $this->client->get('/v1/users/' . $id);
        $this->client->request('POST', "https://api.example.com/v1/items/{$id}?x=1");
        $this->client->requestAsync($method, sprintf('/v1/jobs/%d', $id));
        $url = '/v1/once';
        $this->client->putAsync($url);
        Http::withToken('t')->delete("{$this->base}/v1/tokens/1");
        Http::get('https://h/status');
        $ch = curl_init();
        curl_setopt($ch, CURLOPT_URL, 'http://localhost/ping');
        Storage::disk('s3')->get('/avatars/a.png');
        Storage::delete('/avatars/a.png');
        $request->get('name');
        $this->client->get($id);
    }
}
"#;
    let owner = "App\\Services\\Api::run";
    assert_eq!(
        keys(src, Consumes),
        triples(&[
            ("GET /v1/users/{}", owner, "guzzle"),
            ("POST /v1/items/{}", owner, "guzzle"),
            ("* /v1/jobs/{}", owner, "guzzle"),
            ("PUT /v1/once", owner, "guzzle"),
            ("DELETE /v1/tokens/1", owner, "laravel-http"),
            ("GET /status", owner, "laravel-http"),
            ("* /ping", owner, "curl"),
        ])
    );
}
