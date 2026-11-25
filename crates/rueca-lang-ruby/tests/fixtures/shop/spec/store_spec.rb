require "shop"

RSpec.describe Shop::Store do
  it "sells" do
    Shop::Store.open.sell("a")
  end
end
