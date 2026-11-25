namespace App;

public class Logger
{
    public void Write(string message)
    {
    }
}

public class Job
{
    private Logger _logger;

    public void Run()
    {
        _logger?.Write("start");
    }
}
