namespace App;

public interface IClock
{
    long Now();
}

public class SystemClock : IClock
{
    public long Now()
    {
        return 0;
    }
}

public class Timer
{
    public long Read()
    {
        var clock = new SystemClock();
        return clock.Now();
    }
}
