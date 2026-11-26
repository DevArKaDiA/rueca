namespace App;

public class Pager
{
    public int Page(int size, int start = 0)
    {
        return start;
    }

    public int Page(string token)
    {
        return 0;
    }

    public int First()
    {
        return Page(10);
    }
}
