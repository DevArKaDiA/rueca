namespace App;

public class Cache
{
    public bool TryGet(string key, out Entry entry)
    {
        entry = null;
        return false;
    }
}

public class Entry
{
    public void Touch()
    {
    }
}

public class Reader
{
    public void Read(Cache cache)
    {
        if (cache.TryGet("k", out Entry e))
        {
            e.Touch();
        }
    }
}
