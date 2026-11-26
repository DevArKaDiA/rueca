namespace Acme.Shop.Models
{
    public partial class Order
    {
        public decimal Total()
        {
            Validate();
            return Lines.Count;
        }
    }
}
