namespace App;

public class Options
{
    public int Retries { get; set; }

    public Options()
    {
        Reset();
    }

    public void Reset()
    {
    }
}

public class Client
{
    public Options Defaults()
    {
        return new Options { Retries = 3 };
    }
}
