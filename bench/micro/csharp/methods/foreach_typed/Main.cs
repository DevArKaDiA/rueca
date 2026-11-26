using System.Collections.Generic;

namespace App;

public class Item
{
    public void Ship()
    {
    }
}

public class Order
{
    public void ShipAll(List<Item> items)
    {
        foreach (Item item in items)
        {
            item.Ship();
        }
    }
}
