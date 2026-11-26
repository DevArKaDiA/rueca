namespace App;

public class Pipeline
{
    public int Parse(string s)
    {
        return 1;
    }

    public int Double(int x)
    {
        return x * 2;
    }

    public int Run(string s)
    {
        return Double(Parse(s));
    }
}
