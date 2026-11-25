namespace App;

public interface IStore
{
    void Save(string item);
}

public class FileStore : IStore
{
    public void Save(string item)
    {
    }
}

public class Saver
{
    public void Persist(IStore store)
    {
        store.Save("x");
    }
}
