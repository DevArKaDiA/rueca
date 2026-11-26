using App.Data;

namespace App.Web;

public class Controller
{
    public void Get(Repository repo)
    {
        repo.Load();
    }
}
