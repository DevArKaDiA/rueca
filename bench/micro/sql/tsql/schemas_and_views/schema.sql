CREATE TABLE [dbo].[Customers] (
    [CustomerID] NCHAR(5) NOT NULL PRIMARY KEY
)
GO
CREATE TABLE [Sales].[Orders] (
    [OrderID] INT NOT NULL PRIMARY KEY,
    [CustomerID] NCHAR(5) NOT NULL REFERENCES [dbo].[Customers] ([CustomerID])
)
GO
CREATE VIEW [dbo].[vCustomerOrders]
AS
SELECT c.[CustomerID], o.[OrderID]
FROM [dbo].[Customers] AS c
INNER JOIN [Sales].[Orders] AS o ON o.[CustomerID] = c.[CustomerID]
GO
