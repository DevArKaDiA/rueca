namespace App;

public interface IRunner
{
    void Run();
}

public class Task : IRunner
{
    void IRunner.Run()
    {
        Prepare();
    }

    private void Prepare()
    {
    }
}
