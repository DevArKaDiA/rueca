using App.Billing;

namespace App.Web;

public class Screen
{
    public void Show(Invoice invoice)
    {
        invoice.Total();
    }
}
