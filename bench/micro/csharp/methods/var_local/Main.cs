namespace App;

public class Parser
{
    public string Parse(string s)
    {
        return s.Trim();
    }
}

public class Tool
{
    public string Run()
    {
        var p = new Parser();
        return p.Parse(" x ");
    }
}
