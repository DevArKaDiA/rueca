package app;

class Controller {
    void render(String view) {
    }
}

class HomeController extends Controller {
    void index() {
        render("home");
    }
}
