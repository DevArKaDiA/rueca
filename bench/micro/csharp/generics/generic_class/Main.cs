namespace App;

public class Box<T>
{
    public T Get()
    {
        return default;
    }
}

public class Shelf
{
    public string Take(Box<string> box)
    {
        return box.Get();
    }
}
