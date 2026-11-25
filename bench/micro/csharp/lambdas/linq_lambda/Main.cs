using System.Collections.Generic;
using System.Linq;

namespace App;

public class Product
{
    public bool InStock()
    {
        return true;
    }
}

public class Catalog
{
    public int Available(List<Product> products)
    {
        return products.Count(p => p.InStock());
    }
}
