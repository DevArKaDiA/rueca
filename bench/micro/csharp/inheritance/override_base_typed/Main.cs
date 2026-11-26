namespace App;

public abstract class Handler
{
    public abstract void Handle();
}

public class Ping : Handler
{
    public override void Handle()
    {
    }
}

public class Bus
{
    public void Send(Handler h)
    {
        h.Handle();
    }
}
