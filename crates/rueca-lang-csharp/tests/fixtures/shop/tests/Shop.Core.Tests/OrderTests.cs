using Acme.Shop.Models;
using Xunit;

namespace Acme.Shop.Tests;

public class OrderTests
{
    [Fact]
    public void AddsLines()
    {
        var order = new Order(1);
        order.Add("a", 2);
        Assert.Equal(1, order.Lines.Count);
    }

    [Theory]
    [InlineData(3)]
    public void Totals(int n) => Assert.True(new Order(n).Total() >= 0);
}
