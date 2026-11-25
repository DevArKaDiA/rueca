namespace App;

public class Connection
{
    public Connection(string url)
    {
        Open();
    }

    public void Open()
    {
    }
}

public class Pool
{
    public Connection Create()
    {
        return new Connection("db");
    }
}
