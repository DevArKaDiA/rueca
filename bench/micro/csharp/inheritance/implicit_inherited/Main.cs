namespace App;

public class Base
{
    public void Log()
    {
    }
}

public class Service : Base
{
    public void Run()
    {
        Log();
    }
}
