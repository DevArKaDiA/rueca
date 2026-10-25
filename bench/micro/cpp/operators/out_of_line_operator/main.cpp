struct Money {
    long cents;
    bool operator==(const Money &o) const;
    bool same(const Money &o) const { return operator==(o); }
};

bool Money::operator==(const Money &o) const { return cents == o.cents; }
