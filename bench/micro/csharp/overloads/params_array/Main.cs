namespace App;

public class Log
{
    public void Info(string message)
    {
    }

    public void Info(string format, params object[] args)
    {
    }

    public void Many()
    {
        Info("{0} {1}", 1, 2);
    }
}
