using System.Collections.Generic;
using System.Linq;

namespace App;

public class Names
{
    public int Count(List<string> names)
    {
        return names.Where(n => n.Length > 0).Count();
    }

    public string First(List<string> names)
    {
        return names.First();
    }
}
