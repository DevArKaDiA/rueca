namespace App;

public interface IEntity
{
    int Key();
}

public class Index<T> where T : IEntity
{
    public int Of(T item)
    {
        return item.Key();
    }
}
