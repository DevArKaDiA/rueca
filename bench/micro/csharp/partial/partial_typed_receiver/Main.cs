namespace App;

public class Shop
{
    public int Buy(Cart cart)
    {
        cart.Add();
        return cart.Total();
    }
}
