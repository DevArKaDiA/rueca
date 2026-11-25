using Microsoft.Extensions.Configuration;
using static Acme.Shop.Services.Pricing;

namespace Acme.Shop.Services;

public class OrderService
{
    private readonly IOrderStore _store;
    private readonly IConfiguration _config;

    public OrderService(IOrderStore store, IConfiguration config)
    {
        _store = store;
        _config = config;
    }

    public Order Place(int id)
    {
        var order = new Order(id);
        order.Add("sku-1");
        order.Validate();
        _store.Save(order);
        var rate = _config["Shop:TaxRate"];
        var url = Environment.GetEnvironmentVariable("SHOP_DB");
        Discount(order);
        order.Audit();
        return order;
    }
}

public static class Pricing
{
    public static decimal Discount(Order order) => order.Total() / 10;
}

public static class OrderExtensions
{
    public static void Audit(this Order order) => order.Validate();
}
