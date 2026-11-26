namespace App;

public class Mapper
{
    public T Map<T>(object source)
    {
        return default;
    }

    public string Name(object o)
    {
        return Map<string>(o);
    }
}
