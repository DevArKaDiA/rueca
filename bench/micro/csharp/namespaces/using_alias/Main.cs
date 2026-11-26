using Store = App.Persistence.OrderStore;

namespace App.Web;

public class Checkout
{
    public void Complete(Store store)
    {
        store.Commit();
    }
}
